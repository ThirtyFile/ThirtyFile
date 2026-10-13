//! An incoming request owns its temporary file until it is either discarded or atomically attached to an upload.
//! Dropping the request future cleans it too. Published multipart files are disarmed only after the DB commit.

pub struct Temporary {
    path: std::path::PathBuf,
    keep: bool,
}

impl Temporary {
    pub fn new(st: &crate::state::AppState, prefix: &str) -> Self {
        Self { path: st.tmp_dir().join(format!("s3-{prefix}-{}", crate::util::new_id())), keep: false }
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub async fn rename(&mut self, target: std::path::PathBuf) -> std::io::Result<()> {
        tokio::fs::rename(&self.path, &target).await?;
        self.path = target;
        Ok(())
    }
    pub fn persist(mut self) {
        self.keep = true;
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
