# simple-mem-cache

A simple in-memory cache with an HTTP interface.

## Features
* HTTP POST http://127.0.0.1:8080/<key> with the value as UTF-8 body.
* HTTP GET http://127.0.0.1:8080/<key> replies with the value as body or 404 if no such key exists.
* Uses actix for high performance.
* Uses CHashMap as a backing store so only buckets are locked.
* Configurable logging uses log and log4rs.
* Configutaration via file and environment.
* Built metrics server on port http://127.0.0.1:8081/metrics for Prometheus.
