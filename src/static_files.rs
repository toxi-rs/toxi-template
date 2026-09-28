use toxi_core::{ToxiRequest, ToxiResponse, Error, Result};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::future::Future;
use std::pin::Pin;

const SERVER_HEADER: &str = concat!("Toxi/", env!("CARGO_PKG_VERSION"));

/// Files at or below this size are cached in memory. Larger files stream
/// from disk on every hit so one video cannot evict the working set.
const DEFAULT_MAX_CACHED_FILE_SIZE: u64 = 1024 * 1024;

/// One cached file. `content` clones as an atomic reference count, so
/// hits never copy bytes. `content_type` is a static table entry.
#[derive(Clone)]
struct CachedFile {
    content: bytes::Bytes,
    content_type: &'static str,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

/// Content type table shared by the disk and cache paths.
fn content_type_for(path: &Path) -> &'static str {
    // The lowercase copy is skipped when the extension is already
    // lowercase, which is the common case for served assets.
    let mut ext_lower = String::new();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            if ext.bytes().any(|b| b.is_ascii_uppercase()) {
                ext_lower = ext.to_lowercase();
                ext_lower.as_str()
            } else {
                ext
            }
        });
    ext.map(|ext| match ext {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "txt" => "text/plain",
        "xml" => "text/xml",
        _ => "application/octet-stream",
    })
    .unwrap_or("application/octet-stream")
}

/// Configuration for static file serving
#[derive(Clone)]
pub struct StaticFiles {
    root: String,
    url_prefix: Option<String>,
    max_cached_file_size: u64,
    cache: Arc<RwLock<HashMap<PathBuf, CachedFile>>>,
}

impl StaticFiles {
    /// Create a new StaticFiles handler
    ///
    /// # Arguments
    /// * `root` - The directory on the filesystem to serve files from (e.g., "public")
    /// * `url_prefix` - Optional URL prefix to strip from the request path (e.g., "/public")
    pub fn new(root: impl Into<String>, url_prefix: Option<String>) -> Self {
        Self {
            root: root.into(),
            url_prefix,
            max_cached_file_size: DEFAULT_MAX_CACHED_FILE_SIZE,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Cap per-file cache size in bytes. Files above the cap always
    /// stream from disk. Clears the cache since entries may exceed it.
    pub fn with_max_cached_file_size(mut self, bytes: u64) -> Self {
        self.max_cached_file_size = bytes;
        self.cache.write().expect("static cache lock").clear();
        self
    }

    /// Serve a static file based on the request
    pub async fn serve(&self, req: ToxiRequest) -> Result<ToxiResponse> {
        use hyper::{Response, header};
        use http_body_util::{Full, BodyExt};
        use bytes::Bytes;
        use http::StatusCode;

        let path = req.uri().path();
        
        // Remove prefix if configured
        let file_path = if let Some(prefix) = &self.url_prefix {
            if path.starts_with(prefix) {
                path.strip_prefix(prefix).unwrap_or(path)
            } else {
                path
            }
        } else {
            path
        };

        // Clean up leading slashes to make it relative
        let file_path = file_path.trim_start_matches('/');
        
        // Security: prevent directory traversal
        if file_path.contains("..") {
            return Err(Error::BadRequest("Invalid path".to_string()));
        }
        
        let full_path = Path::new(&self.root).join(file_path);
        
        // Check if path is a directory, if so try index.html
        let full_path = if full_path.is_dir() {
            full_path.join("index.html")
        } else {
            full_path
        };

        // One stat against a full disk read: serve from memory when the
        // file is cached and unchanged. A concurrent edit changes mtime
        // or length, which fails the check below and re-reads from disk,
        // so stale entries heal on the next hit.
        let meta = tokio::fs::metadata(&full_path).await.ok();
        if let Some(meta) = &meta {
            if meta.is_file() && meta.len() <= self.max_cached_file_size {
                let modified = meta.modified().ok();
                let hit = {
                    self.cache
                        .read()
                        .expect("static cache lock")
                        .get(&full_path)
                        .cloned()
                };
                if let Some(entry) = hit {
                    if entry.len == meta.len() && entry.modified == modified {
                        return bytes_response(entry.content, entry.content_type);
                    }
                }
            }
        }

        // Read file asynchronously as bytes
        match tokio::fs::read(&full_path).await {
            Ok(content) => {
                let content_type = content_type_for(&full_path);
                let bytes = bytes::Bytes::from(content);
                // Refresh the cache for small files. The metadata taken
                // above describes this content unless the file changed
                // mid-read, in which case the next hit mismatches and
                // re-reads rather than serving stale bytes.
                if (bytes.len() as u64) <= self.max_cached_file_size {
                    if let Some(meta) = &meta {
                        if meta.is_file() {
                            self.cache.write().expect("static cache lock").insert(
                                full_path,
                                CachedFile {
                                    content: bytes.clone(),
                                    content_type,
                                    len: meta.len(),
                                    modified: meta.modified().ok(),
                                },
                            );
                        }
                    }
                }
                bytes_response(bytes, content_type)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Return 404 Not Found
                let res = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .header(header::SERVER, SERVER_HEADER)
                    .body(Full::new(Bytes::from("404 Not Found")).map_err(|e| match e {}).boxed())
                    .map_err(|e| Error::InternalServerError(format!("Failed to build response: {}", e)))?;
                
                Ok(ToxiResponse::new(res))
            },
            Err(e) => {
                // Return 500 Internal Server Error for other errors
                Err(Error::InternalServerError(format!("Failed to read file: {}", e)))
            }
        }
    }
}

/// Build a 200 response from in-memory bytes.
fn bytes_response(content: bytes::Bytes, content_type: &'static str) -> Result<ToxiResponse> {
    use hyper::{Response, header};
    use http_body_util::{Full, BodyExt};
    use http::StatusCode;

    let res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, content.len())
        .header(header::SERVER, SERVER_HEADER)
        .body(Full::new(content).map_err(|e| match e {}).boxed())
        .map_err(|e| Error::InternalServerError(format!("Failed to build response: {}", e)))?;

    Ok(ToxiResponse::new(res))
}

/// Create a static file handler for a specific directory.
/// 
/// # Example
/// ```ignore
/// use toxi_template::static_handler;
///
/// // Register in your router:
/// // router.get("/assets/*", static_handler("public"));
/// ```
pub fn static_handler(root: impl Into<String>) -> impl Fn(ToxiRequest) -> Pin<Box<dyn Future<Output = Result<ToxiResponse>> + Send>> + Send + Sync + 'static {
    let root = root.into();
    let static_files = Arc::new(StaticFiles::new(root, None));
    
    move |req| {
        let static_files = static_files.clone();
        Box::pin(async move {
            static_files.serve(req).await
        })
    }
}

/// Helper function to serve static files from the "public" directory.
/// 
/// This handler serves files relative to the root of the "public" directory.
/// For example, a request to `/style.css` will serve `public/style.css`.
pub async fn serve_static(req: ToxiRequest) -> Result<ToxiResponse> {
    let static_files = StaticFiles::new("public", None);
    static_files.serve(req).await
}
