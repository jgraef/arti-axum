# arti-axum

This is not a part of the arti project.

This crate allows you to run your axum http server as a tor hidden service.

## Example

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
