use crate::state::ServiceLifecycle;

use axum::Router;
use std::{
    future::{Future, IntoFuture},
    io,
    net::SocketAddr,
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch, time};

pub async fn serve(
    listener: TcpListener,
    app: Router,
    lifecycle: ServiceLifecycle,
    shutdown_timeout: Duration,
) -> io::Result<()> {
    serve_with_signal(
        listener,
        app,
        lifecycle,
        wait_for_shutdown_signal(),
        shutdown_timeout,
    )
    .await
}

async fn serve_with_signal<F>(
    listener: TcpListener,
    app: Router,
    lifecycle: ServiceLifecycle,
    shutdown_signal: F,
    shutdown_timeout: Duration,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut deadline_receiver = shutdown_rx.clone();
    let signal_lifecycle = lifecycle.clone();
    let signal_task = tokio::spawn(async move {
        shutdown_signal.await;
        signal_lifecycle.set_ready(false);
        tracing::warn!("shutdown signal received; draining active requests");
        let _ = shutdown_tx.send(true);
    });

    let graceful_shutdown = async move {
        let mut receiver = shutdown_rx;
        while !*receiver.borrow() && receiver.changed().await.is_ok() {}
    };

    let deadline = async move {
        while !*deadline_receiver.borrow() && deadline_receiver.changed().await.is_ok() {}
        time::sleep(shutdown_timeout).await;
    };

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(graceful_shutdown)
    .into_future();
    tokio::pin!(server);
    tokio::pin!(deadline);

    tokio::select! {
        result = &mut server => {
            signal_task.abort();
            result
        }
        _ = &mut deadline => {
            signal_task.abort();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "graceful shutdown deadline expired while requests were still active",
            ))
        }
    }
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            tracing::error!(error = %error, "could not listen for SIGINT");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "could not listen for SIGTERM; waiting for SIGINT");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::error!(error = %error, "could not listen for SIGINT");
                    std::future::pending::<()>().await;
                }
            }
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %error, "could not listen for Ctrl-C");
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::routing::get;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::oneshot,
    };

    async fn delayed_response() -> &'static str {
        time::sleep(Duration::from_millis(100)).await;
        "finished"
    }

    async fn never_finishes() -> &'static str {
        std::future::pending().await
    }

    async fn request(addr: SocketAddr) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("client request should be sent");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("client response should be readable");
        response
    }

    #[tokio::test]
    async fn graceful_shutdown_finishes_active_requests_and_marks_not_ready() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have an address");
        let lifecycle = ServiceLifecycle::default();
        lifecycle.set_ready(true);
        let app = Router::new().route("/", get(delayed_response));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_lifecycle = lifecycle.clone();

        let server = tokio::spawn(async move {
            serve_with_signal(
                listener,
                app,
                server_lifecycle,
                async move {
                    let _ = shutdown_rx.await;
                },
                Duration::from_secs(1),
            )
            .await
        });

        let request_task = tokio::spawn(request(addr));
        time::sleep(Duration::from_millis(25)).await;
        shutdown_tx
            .send(())
            .expect("server should still be running");

        let response = time::timeout(Duration::from_secs(2), request_task)
            .await
            .expect("active request should drain before timeout")
            .expect("request task should complete");
        assert!(String::from_utf8_lossy(&response).contains("finished"));
        assert!(server.await.expect("server task should complete").is_ok());
        assert!(!lifecycle.is_ready());
    }

    #[tokio::test]
    async fn shutdown_deadline_abandons_requests_that_do_not_finish() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have an address");
        let lifecycle = ServiceLifecycle::default();
        lifecycle.set_ready(true);
        let app = Router::new().route("/", get(never_finishes));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_lifecycle = lifecycle.clone();

        let server = tokio::spawn(async move {
            serve_with_signal(
                listener,
                app,
                server_lifecycle,
                async move {
                    let _ = shutdown_rx.await;
                },
                Duration::from_millis(50),
            )
            .await
        });

        let request_task = tokio::spawn(request(addr));
        time::sleep(Duration::from_millis(25)).await;
        shutdown_tx
            .send(())
            .expect("server should still be running");

        let error = time::timeout(Duration::from_secs(1), server)
            .await
            .expect("shutdown deadline should be enforced")
            .expect("server task should complete")
            .expect_err("unfinished requests should exceed the deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(!lifecycle.is_ready());
        let _ = time::timeout(Duration::from_millis(250), request_task).await;
    }
}
