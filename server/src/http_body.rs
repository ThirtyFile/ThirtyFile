//! Receiving large request bodies: healthy transfers may take arbitrarily long, but idle clients release resources.

use crate::error::{AppError, AppResult};
use axum::{
    body::{BodyDataStream, Bytes},
    http::StatusCode,
};
use futures_util::StreamExt;
use std::time::Duration;

pub const UPLOAD_IDLE: Duration = Duration::from_secs(120);

/// The deadline is per read, never the duration of the whole upload or its storage writes.
pub async fn next_chunk(stream: &mut BodyDataStream) -> AppResult<Option<Bytes>> {
    let read = async {
        loop {
            match stream.next().await {
                Some(Ok(bytes)) if bytes.is_empty() => tokio::task::yield_now().await,
                Some(Ok(bytes)) => return Ok(Some(bytes)),
                Some(Err(_)) => return Err(AppError::bad_request("Connection interrupted")),
                None => return Ok(None),
            }
        }
    };
    match tokio::time::timeout(UPLOAD_IDLE, read).await {
        Ok(result) => result,
        Err(_) => Err(AppError::new(StatusCode::REQUEST_TIMEOUT, "Upload timed out while waiting for data. Resume or retry the upload.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    #[tokio::test(start_paused = true)]
    async fn an_idle_body_times_out_after_partial_progress() {
        let first = futures_util::stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"x")) });
        let mut stream = Body::from_stream(first.chain(futures_util::stream::pending())).into_data_stream();
        assert_eq!(next_chunk(&mut stream).await.unwrap().unwrap(), "x");
        let error = next_chunk(&mut stream).await.unwrap_err();
        assert_eq!(error.status, StatusCode::REQUEST_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn continuing_progress_resets_the_deadline_for_a_long_upload() {
        let chunks = futures_util::stream::unfold(0, |i| async move {
            if i == 4 {
                return None;
            }
            tokio::time::sleep(Duration::from_secs(90)).await;
            Some((Ok::<_, std::io::Error>(Bytes::from_static(b"x")), i + 1))
        });
        let mut stream = Body::from_stream(chunks).into_data_stream();
        for _ in 0..4 {
            assert!(next_chunk(&mut stream).await.unwrap().is_some());
        }
        assert!(next_chunk(&mut stream).await.unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn empty_chunks_do_not_keep_an_idle_upload_alive() {
        let chunks = futures_util::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Some((Ok::<_, std::io::Error>(Bytes::new()), ()))
        });
        let mut stream = Body::from_stream(chunks).into_data_stream();
        assert_eq!(next_chunk(&mut stream).await.unwrap_err().status, StatusCode::REQUEST_TIMEOUT);
    }

    #[tokio::test]
    async fn an_interrupted_body_keeps_the_existing_connection_error() {
        let chunks = futures_util::stream::once(async { Err::<Bytes, _>(std::io::Error::other("disconnected")) });
        let mut stream = Body::from_stream(chunks).into_data_stream();
        assert_eq!(next_chunk(&mut stream).await.unwrap_err().message, "Connection interrupted");
    }
}
