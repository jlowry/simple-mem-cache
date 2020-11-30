mod cache;
mod settings;
use crate::cache::{CacheMetrics, SimpleCache};
use crate::settings::Settings;
use actix_web::{get, middleware, post, rt::System, web, App, HttpResponse, HttpServer};
use actix_web_prom::PrometheusMetrics;
use prometheus::Registry;
use std::{io, thread, thread::JoinHandle, time::Duration};

#[get("/{key}")]
async fn index_get<'a>(key: web::Path<String>, cache: web::Data<SimpleCache<'a>>) -> HttpResponse {
    match cache.get(key.into_inner(), &|value| HttpResponse::Ok().body(value)) {
        Some(value) => value,
        None => HttpResponse::NotFound().finish(),
    }
}

#[post("/{key}")]
async fn index_post<'a>(
    key: web::Path<String>,
    value: String,
    cache: web::Data<SimpleCache<'a>>,
) -> HttpResponse {
    cache.put(key.into_inner(), value);
    HttpResponse::Ok().finish()
}

fn start_cache_cleaner(cache: web::Data<SimpleCache<'static>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut sys = System::new("cleaner");
        let cleaner = SimpleCache::cleaner(cache.clone());
        sys.block_on(cleaner);
    })
}

fn start_cache_server(
    settings: settings::HttpServer,
    cache: web::Data<SimpleCache<'static>>,
    http_metrics: PrometheusMetrics,
) -> JoinHandle<()> {
    thread::spawn(|| {
        let mut sys = System::new("cache_server");
        let mut cache_server = HttpServer::new(move || {
            App::new()
                .app_data(cache.clone()) // add shared state
                .wrap(http_metrics.clone())
                .wrap(middleware::Logger::default())
                .service(index_get)
                .service(index_post)
        });
        config_items! {
            cache_server = settings;
            workers,
            backlog,
            max_connections,
            max_connection_rate,
            client_timeout,
            client_shutdown,
            shutdown_timeout
        };
        for socket_addr in settings.listen_addresses {
            cache_server = cache_server.bind(socket_addr).unwrap();
        }
        let srv = cache_server.run();
        sys.block_on(srv).unwrap();
    })
}

fn configure_metrics(registry: Registry) -> (PrometheusMetrics, PrometheusMetrics) {
    let http_metrics_with_api = PrometheusMetrics::new_with_registry(
        registry.clone(),
        "private_api",
        Some("/metrics"),
        None,
    )
    // It is safe to unwrap when __no other app has the same namespace__
    .unwrap();
    let http_metrics = PrometheusMetrics::new_with_registry(
        registry.clone(),
        "public_api",
        // Metrics should not be available from the outside
        None,
        None,
    )
    .unwrap();
    (http_metrics, http_metrics_with_api)
}

fn start_metrics_server(
    settings: settings::HttpServer,
    http_metrics_with_api: PrometheusMetrics,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut sys = System::new("metrics_server");
        let mut metrics_server = HttpServer::new(move || {
            App::new()
                .wrap(http_metrics_with_api.clone())
                .wrap(middleware::Logger::default())
        });

        config_items! {
            metrics_server = settings;
            workers,
            backlog,
            max_connections,
            max_connection_rate,
            client_timeout,
            client_shutdown,
            shutdown_timeout
        };
        for socket_addr in settings.listen_addresses {
            metrics_server = metrics_server.bind(socket_addr).unwrap();
        }

        let srv = metrics_server.run();
        sys.block_on(srv).unwrap();
    })
}

fn main() -> std::io::Result<()> {
    let Settings {
        cache: cache_settings,
        cache_server: cache_server_settings,
        metrics_server: metrics_server_settings,
        logger_config_file,
    } = match Settings::new() {
        Ok(settings) => settings,
        Err(err) => return Err(io::Error::new(io::ErrorKind::Other, err)),
    };

    log4rs::init_file(logger_config_file, Default::default()).unwrap();

    let registry = prometheus::default_registry();
    let (http_metrics, http_metrics_with_api) = configure_metrics(registry.clone());

    let key_live_duration = Duration::from_secs(cache_settings.key_live_duration);
    let cache_metrics = CacheMetrics::new();
    cache_metrics.register(registry);
    let cache = web::Data::new(SimpleCache::new(key_live_duration, cache_metrics));

    start_cache_cleaner(cache.clone());
    let thread_cache_server = start_cache_server(cache_server_settings, cache, http_metrics);
    let thread_metrics_server =
        start_metrics_server(metrics_server_settings, http_metrics_with_api);

    thread_cache_server.join().unwrap();
    thread_metrics_server.join().unwrap();
    Ok(())
}
