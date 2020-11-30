use actix_rt::time::{delay_for, Delay};
use chashmap::CHashMap;
use crossbeam_channel::{unbounded, Receiver, Sender};
use prometheus::{IntCounterVec, IntGauge, Opts, Registry};
use std::{
    borrow::Cow,
    ops::Deref,
    sync::Arc,
    time::{Duration, Instant},
};

/// Container for the cache metrics.
#[derive(Clone)]
pub struct CacheMetrics {
    /// A Count of cache hits and misses.
    pub queries: IntCounterVec,
    /// The number of items in the cache.
    pub items: IntGauge,
    /// The size in byts of values (not keys or expiry info) stored in the cache.
    pub size: IntGauge,
}

impl CacheMetrics {
    /// Creates a new CacheMetrics.
    pub fn new() -> Self {
        Self {
            queries: IntCounterVec::new(
                Opts::new("cache_query", "A count of cache hits and misses"),
                &["hit_or_miss"],
            )
            .unwrap(),
            items: IntGauge::new("cache_items", "The number of item in the cache").unwrap(),
            size: IntGauge::new(
                "cache_size",
                "The total size in bytes of all values in the cache",
            )
            .unwrap(),
        }
    }

    /// Registery the metrics contained in CacheMetrics with a registry.
    pub fn register(&self, resgistry: &Registry) {
        resgistry.register(Box::new(self.queries.clone())).unwrap();
        resgistry.register(Box::new(self.items.clone())).unwrap();
        resgistry.register(Box::new(self.size.clone())).unwrap();
        log::info!("Registered cache metrics");
    }
}

struct CacheValue {
    value: String,
    expiry: Instant,
}

struct KeyExpiry<'a>(Cow<'a, str>, Instant);

/// A cache based around CHashMap.
pub struct SimpleCache<'a> {
    key_live_duration: Duration,
    backing_store: CHashMap<Cow<'a, str>, CacheValue>,
    sender: Sender<KeyExpiry<'a>>,
    receiver: Receiver<KeyExpiry<'a>>,
    metrics: CacheMetrics,
}

impl<'a> SimpleCache<'a> {
    /// Returns a new `SimpleCache`.
    /// # Arguments
    /// * `key_live_duration` - The `Duration` a key exists within the cache.
    /// * `metrics` - A container for the metrics used by the cache.
    pub fn new(key_live_duration: Duration, metrics: CacheMetrics) -> Self {
        let (sender, receiver) = unbounded();
        Self {
            key_live_duration,
            sender,
            receiver,
            backing_store: CHashMap::default(),
            metrics,
        }
    }

    /// Removes a key from the cache if the `CacheValue::expiry` is older than the supplied expiry.
    /// # Arguments
    /// * `key` - The key to remove.
    /// * `expiry` - The `Instant` to test against.
    fn remove_key_if_older_than(&self, key: Cow<'a, str>, expiry: Instant) {
        self.backing_store
            .alter(key.clone(), |maybe_value| match maybe_value {
                Some(value) if value.expiry > expiry => Some(value),
                Some(value) => {
                    log::debug!("Removed expired key from cache: {}", key);
                    self.metrics.items.set(self.len() as i64);
                    self.metrics.size.sub(value.value.len() as i64);
                    None
                }
                None => None,
            });
    }

    /// Processes expired keys until it receives a key that is not expired or there are no keys
    /// available.
    /// # Arguments
    /// * `delay` - A function that generates a delay.
    async fn clean(&self, delay: fn(Duration) -> Delay) {
        for KeyExpiry(key, expiry) in self.receiver.try_iter() {
            let now = Instant::now();
            if expiry > now {
                delay(expiry - now).await;
            }
            self.remove_key_if_older_than(key, expiry);
        }
    }

    /// Runs the clean method until there are no keys available and then delays for the
    /// key_live_duration.
    /// # Arguments
    /// * `simple_cache` - The cache to clean. It is type that Derefs to Arc<SimpleCache> to be
    /// compatible with actix_web.
    pub async fn cleaner<C: Deref<Target = Arc<Self>>>(simple_cache: C) {
        log::info!("Starting cache cleaner");
        loop {
            simple_cache.clean(delay_for).await;
            delay_for(simple_cache.key_live_duration).await;
        }
    }

    fn len(&self) -> usize {
        self.backing_store.len()
    }

    /// Returns the value mapped using `as_value` or None.
    /// # Arguments
    /// * `key` - The cache key.
    /// * `as_value` - A mapping function.
    pub fn get<K, V>(&self, key: K, as_value: &dyn Fn(&String) -> V) -> Option<V>
    where
        K: Into<Cow<'a, str>>,
    {
        let key: Cow<'a, str> = key.into();
        match self.backing_store.get(&key) {
            Some(v) => {
                log::debug!("Cache hit for key: {}", key);
                self.metrics.queries.with_label_values(&["hit"]).inc();
                Some(as_value(&v.value))
            }
            None => {
                log::debug!("Cache miss for key: {}", key);
                self.metrics.queries.with_label_values(&["miss"]).inc();
                None
            }
        }
    }

    /// Adds a value to the cache and sets it's expiry to `now()` +  `key_live_duration`
    /// # Arguments
    /// * `key` - The cache key.
    /// * `value` - The value to be stored in the cache.
    pub fn put<K>(&self, key: K, value: String)
    where
        K: Into<Cow<'a, str>>,
    {
        let key: Cow<'a, str> = key.into();
        let expiry = Instant::now() + self.key_live_duration;
        let value_size = value.len();
        if let Some(old_value) = self
            .backing_store
            .insert(key.clone(), CacheValue { value, expiry })
        {
            self.metrics.size.sub(old_value.value.len() as i64);
        }
        log::debug!("Added key: {} with expiry: {:?} to cache", key, expiry);
        self.metrics.items.set(self.len() as i64);
        self.metrics.size.add(value_size as i64);
        if let Err(err) = self.sender.send(KeyExpiry(key, expiry)) {
            log::error!("Could not add key to expiry queue. {}", err);
        };
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use actix_web::web;
    use futures::join;
    use std::thread;

    #[test]
    fn cach_hit_returns_value() {
        let (sut, _) = new_cache();

        sut.put("", "".to_string());
        let result = sut.get("", &|v| v.clone());

        assert_eq!(result, Some("".to_string()));
    }

    #[test]
    fn cache_miss_returns_none() {
        let (sut, _) = new_cache();

        let result = sut.get("bar", &|v| v.clone());

        assert_eq!(result, None);
    }

    #[actix_rt::test]
    async fn expired_items_are_removed_from_the_cache() {
        let (sut, _) = new_cache();

        sut.put("", "".to_string());
        thread::sleep(Duration::from_millis(5));

        sut.clean(delay_for).await;
        let result = sut.get("", &|v| v.clone());

        assert_eq!(result, None);
    }

    #[actix_rt::test]
    async fn items_that_are_updated_with_new_value_do_not_expire_on_previous_expiry() {
        let (sut, _) = new_cache();

        sut.put("", "old_value".to_string());
        thread::sleep(Duration::from_millis(5));
        sut.put("", "new_value".to_string());
        let (_, result) = join!(sut.clean(delay_for), async { sut.get("", &|v| v.clone()) });

        assert_eq!(result, Some("new_value".to_string()));
    }

    #[test]
    fn unexpired_values_are_not_removed() {
        let (sut, _) = new_cache();

        sut.put("", "old_value".to_string());

        sut.remove_key_if_older_than("".into(), Instant::now());
        let result = sut.get("", &|v| v.clone());

        assert_eq!(result, Some("old_value".to_string()));
    }

    #[test]
    fn expired_values_are_removed() {
        let (sut, _) = new_cache();

        sut.put("", "old_value".to_string());

        sut.remove_key_if_older_than("".into(), Instant::now() + Duration::from_millis(5));
        let result = sut.get("", &|v| v.clone());

        assert_eq!(result, None);
    }

    #[test]
    fn key_that_does_not_exist_does_not_add_anything_to_cache() {
        let (sut, _) = new_cache();

        sut.remove_key_if_older_than("".into(), Instant::now() + Duration::from_millis(5));
        let result = sut.get("", &|v| v.clone());

        assert_eq!(result, None);
    }

    #[test]
    fn metrics_query_hit_is_incremented() {
        let (sut, metrics) = new_cache();

        sut.put("", "".to_string());
        let _ = sut.get("", &|v| v.clone());

        assert_eq!(
            metrics
                .queries
                .get_metric_with_label_values(&["hit"])
                .unwrap()
                .get(),
            1
        );
    }

    #[test]
    fn metrics_query_miss_is_incremented() {
        let (sut, metrics) = new_cache();

        let _ = sut.get("", &|v| v.clone());

        assert_eq!(
            metrics
                .queries
                .get_metric_with_label_values(&["miss"])
                .unwrap()
                .get(),
            1
        );
    }

    #[test]
    fn metrics_cache_put_increments_items() {
        let (sut, metrics) = new_cache();

        sut.put("", "".to_string());

        assert_eq!(metrics.items.get(), 1);
    }

    #[test]
    fn metrics_cache_put_increases_size() {
        let (sut, metrics) = new_cache();
        let value = "AAA".to_string();
        let expected = value.len() as i64;

        sut.put("", value);

        assert_eq!(metrics.size.get(), expected);
    }

    #[test]
    fn metrics_cache_put_replacing_a_value_adjusts_size() {
        let (sut, metrics) = new_cache();
        let value1 = "AAAAA".to_string();
        let value2 = "BB".to_string();
        let expected = value2.len() as i64;

        sut.put("", value1);
        sut.put("", value2);

        assert_eq!(metrics.size.get(), expected);
    }

    fn new_cache() -> (web::Data<SimpleCache<'static>>, CacheMetrics) {
        let metrics = CacheMetrics::new();
        let cache = web::Data::new(SimpleCache::new(Duration::from_millis(4), metrics.clone()));
        (cache, metrics)
    }
}
