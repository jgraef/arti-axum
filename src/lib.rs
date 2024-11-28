//!
//! This crate allows you to run your [axum][1] http server as a tor hidden
//! service using [arti][2].
//!
//! ## Example
//!
//! ```rust
//! # use arti_client::{TorClient, TorClientConfig};
//! # use axum::{routing, Router};
//! # use tor_hsservice::{config::OnionServiceConfigBuilder, handle_rend_requests};
//! # fn main() {
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let tor_client = TorClient::create_bootstrapped(TorClientConfig::default()).await?;
//!
//! let (onion_service, rend_requests) = tor_client.launch_onion_service(
//!     OnionServiceConfigBuilder::default()
//!         .nickname("hello-world".to_owned().try_into().unwrap())
//!         .build()?,
//! )?;
//!
//! let stream_requests = handle_rend_requests(rend_requests);
//!
//! let app = Router::new().route("/", routing::get(|| async { "Hello, World!" }));
//!
//! println!("serving at: http://{}", onion_service.onion_name().unwrap());
//!
//! arti_axum::serve(stream_requests, app).await;
//! # Ok(())
//! # }
//! # example(); // we're intentionally not polling the future
//! # }
//! ```
//!
//! [1]: https://docs.rs/axum/latest/axum/index.html
//! [2]: https://gitlab.torproject.org/tpo/core/arti/
//!  

use std::{
    convert::Infallible,
    future::{
        poll_fn,
        Future,
        IntoFuture,
    },
    marker::PhantomData,
    pin::Pin,
    task::{
        Context,
        Poll,
    },
};

use axum::{
    body::Body,
    extract::Request,
    response::Response,
    Router,
};
use futures_util::{
    future::{
        BoxFuture,
        FutureExt,
    },
    stream::{
        BoxStream,
        Stream,
        StreamExt,
    },
};
use hyper::body::Incoming;
use hyper_util::{
    rt::{
        TokioExecutor,
        TokioIo,
    },
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::StreamRequest;
use tor_proto::stream::{
    DataStream,
    IncomingStreamRequest,
};
use tower_service::Service;

/// Serve the service with the supplied stream requests.
///
/// See the [crate documentation](`crate`) for an example.
pub fn serve<M, S>(
    stream_requests: impl Stream<Item = StreamRequest> + Send + 'static,
    make_service: M,
) -> Serve<M, S>
where
    M: for<'a> Service<IncomingStream<'a>, Error = Infallible, Response = S>,
    S: Service<Request, Error = Infallible, Response = Response>,
{
    Serve {
        stream_requests: stream_requests.boxed(),
        make_service,
        _marker: PhantomData,
    }
}

/// Future returned by [`serve`].
pub struct Serve<M, S> {
    stream_requests: BoxStream<'static, StreamRequest>,
    make_service: M,
    _marker: PhantomData<S>,
}

impl<M, S> IntoFuture for Serve<M, S>
where
    M: for<'a> Service<IncomingStream<'a>, Error = Infallible, Response = S> + Send + 'static,
    for<'a> <M as Service<IncomingStream<'a>>>::Future: Send,
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send,
{
    type Output = ();
    type IntoFuture = private::ServeFuture;

    fn into_future(mut self) -> Self::IntoFuture {
        private::ServeFuture {
            inner: async move {
                while let Some(stream_request) = self.stream_requests.next().await {
                    let data_stream = match stream_request.request() {
                        IncomingStreamRequest::Begin(_) => {
                            match stream_request.accept(Connected::new_empty()).await {
                                Ok(data_stream) => data_stream,
                                Err(error) => {
                                    tracing::trace!("failed to accept incoming stream: {error}");
                                    continue;
                                }
                            }
                        }
                        _ => {
                            // we only accept BEGIN streams
                            continue;
                        }
                    };

                    poll_fn(|cx| self.make_service.poll_ready(cx))
                        .await
                        .unwrap_or_else(|err| match err {});

                    let incoming_stream = IncomingStream {
                        data_stream: &data_stream,
                    };

                    let tower_service = self
                        .make_service
                        .call(incoming_stream)
                        .await
                        .unwrap_or_else(|err| match err {});

                    let transformed_service = MapRequestBodyService {
                        inner: tower_service,
                    };

                    let hyper_service = TowerToHyperService::new(transformed_service);

                    tokio::spawn(async move {
                        match Builder::new(TokioExecutor::new())
                            // upgrades needed for websockets
                            .serve_connection_with_upgrades(
                                TokioIo::new(data_stream),
                                hyper_service,
                            )
                            .await
                        {
                            Ok(()) => {}
                            Err(_err) => {
                                // This error only appears when the client
                                // doesn't send a request and
                                // terminate the connection.
                                //
                                // If client sends one request then terminate
                                // connection whenever, it doesn't
                                // appear.
                            }
                        }
                    });
                }
            }
            .boxed(),
        }
    }
}

mod private {
    use super::*;

    pub struct ServeFuture {
        pub inner: BoxFuture<'static, ()>,
    }

    impl Future for ServeFuture {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.inner.poll_unpin(cx)
        }
    }
}

#[derive(Clone)]
struct MapRequestBodyService<S> {
    inner: S,
}

/// A Tower service that transforms the body of incoming requests from
/// `Incoming` to `Body` and delegates the transformed request to the inner
/// service.
impl<S> Service<Request<Incoming>> for MapRequestBodyService<S>
where
    S: Service<Request<Body>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let req = req.map(Body::new);
        self.inner.call(req)
    }
}

/// An incoming stream.
///
/// This is a single client connecting over the TOR network to your onion
/// service.
pub struct IncomingStream<'a> {
    // in the future we can use this to return information about the circuit used etc.
    #[allow(dead_code)]
    data_stream: &'a DataStream,
}

impl Service<IncomingStream<'_>> for Router<()> {
    type Response = Router;
    type Error = Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: IncomingStream<'_>) -> Self::Future {
        std::future::ready(Ok(self.clone()))
    }
}
