# `arti-axum`

[![crates.io](https://img.shields.io/crates/v/arti-axum.svg)](https://crates.io/crates/arti-axum)
[![Documentation](https://docs.rs/arti-axum/badge.svg)](https://docs.rs/arti-axum)
[![MIT](https://img.shields.io/crates/l/arti-axum.svg)](./LICENSE)

This crate allows you to run your [axum][1] http server as a tor hidden service using [arti][2].

## Example

For a full example, take a look at [`hello_world.rs`](examples/hello_world.rs).

```rust
let tor_client = TorClient::create_bootstrapped(TorClientConfig::default()).await?;

let (onion_service, rend_requests) = tor_client.launch_onion_service(
    OnionServiceConfigBuilder::default()
        .nickname("hello-world".to_owned().try_into().unwrap())
        .build()?,
)?;

let stream_requests = handle_rend_requests(rend_requests);

let app = Router::new().route("/", routing::get(|| async { "Hello, World!" }));

println!("serving at: http://{}", onion_service.onion_name().unwrap());

arti_axum::serve(stream_requests, app).await;
```

[1]: https://docs.rs/axum/latest/axum/index.html
[2]: https://gitlab.torproject.org/tpo/core/arti/
