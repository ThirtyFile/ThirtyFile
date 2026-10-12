//! PUT: receiving a file and storing it (`crate::access::write`)

use super::*;

/// Creates or replaces a file. The body is received into a temporary file first, then stored like an upload
pub(super) async fn put(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let (parent, name) = parent_and_name(&mut *st.db.acquire().await?, user, segs).await?;
    let upload = access::upload_to(st, user, &parent, name).await?;
    if upload.existing.is_some() && headers.get(header::IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == b"*") {
        return Err(precondition("The file already exists"));
    }
    let length = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let created = access::write(st, user, upload, body.into_data_stream(), length).await?;
    Ok(if created { StatusCode::CREATED } else { StatusCode::NO_CONTENT }.into_response())
}
