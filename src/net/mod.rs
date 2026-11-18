//! 远端图片加载使用的 HTTP 客户端集成。

use std::io::{self, Read as _};
use std::str::FromStr;
use std::sync::Arc;

use futures::AsyncReadExt;
use futures::FutureExt;
use futures::channel::oneshot;
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, CACHE_CONTROL, HeaderMap, HeaderValue, PRAGMA, USER_AGENT,
};
use rgpui::App;
use rgpui::http_client::{self, AsyncBody, HttpClient};

const IMAGE_ACCEPT_HEADER: &str =
    "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8";
const ACCEPT_LANGUAGE_VALUE: &str = "zh-CN,zh;q=0.9,en-US;q=0.8,en;q=0.7";
const DEFAULT_CACHE_CONTROL: &str = "no-cache";
/// 图片请求超时，避免慢或挂起的远端长期占用工作线程。
const IMAGE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const IMAGE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 图片请求工作线程数上限（避免每个请求都新建裸线程）。
const IMAGE_WORKER_THREADS: usize = 4;
/// 远端图片响应体上限：错配或恶意的 URL 不应把任意大的正文读进内存。
const MAX_IMAGE_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// 图片请求的浏览器 UA（渲染加载与导出抓取共用）。
const IMAGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";

/// 安装全局 HTTP 客户端供图片加载使用。
pub(crate) fn install_http_client(cx: &mut App) {
    match HttpTransportClient::new(IMAGE_USER_AGENT) {
        Ok(client) => cx.set_http_client(Arc::new(client)),
        Err(error) => eprintln!("failed to install HTTP client for image loading: {error}"),
    }
}

/// 同步抓取远程图片字节（导出嵌图用，仅限后台线程调用）。
///
/// 复用渲染侧的请求头、超时与响应体上限；非 2xx 视为失败。
#[cfg(any(feature = "export-pdf", feature = "export-docx"))]
pub(crate) fn fetch_image_bytes_blocking(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(IMAGE_CONNECT_TIMEOUT)
        .timeout(IMAGE_REQUEST_TIMEOUT)
        .build()?;
    let response = client
        .get(url)
        .headers(default_image_request_headers(IMAGE_USER_AGENT)?)
        .send()?;
    if !response.status().is_success() {
        anyhow::bail!(
            "remote image request failed with status {}",
            response.status()
        );
    }
    read_capped_response(response)
}

/// 判断图片来源是否为远端 HTTP 地址。
pub(crate) fn is_remote_image_source(source: &str) -> bool {
    http_client::Uri::from_str(source)
        .ok()
        .and_then(|uri| uri.scheme_str().map(str::to_owned))
        .is_some_and(|scheme| scheme == "http" || scheme == "https")
}

/// 图片请求的固定大小工作线程池。
///
/// 之前每个请求都 `std::thread::spawn` 一条裸线程，图片多的文档会瞬间拉起
/// 大量线程且无法回收；改为固定线程数，任务在池内排队。
struct ImageWorkerPool {
    sender: std::sync::Mutex<std::sync::mpsc::Sender<Box<dyn FnOnce() + Send + 'static>>>,
}

impl ImageWorkerPool {
    /// 创建固定大小线程池。
    fn new(workers: usize) -> anyhow::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send + 'static>>();
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        for index in 0..workers.max(1) {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("rumark-image-http-{index}"))
                .spawn(move || {
                    loop {
                        // 先取任务再放锁：同一时刻只有一个线程在等待，其余在执行任务。
                        let job = {
                            let guard = receiver.lock().expect("image worker lock poisoned");
                            guard.recv()
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_) => break,
                        }
                    }
                })
                .map_err(|err| anyhow::anyhow!("failed to spawn image HTTP worker: {err}"))?;
        }
        Ok(Self {
            sender: std::sync::Mutex::new(sender),
        })
    }

    /// 提交一个任务到线程池。
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        let sender = self.sender.lock().expect("image worker sender poisoned");
        let _ = sender.send(job);
    }
}

/// 基于 reqwest 阻塞传输桥接 GPUI `HttpClient` 接口。
struct HttpTransportClient {
    client: reqwest::blocking::Client,
    user_agent: HeaderValue,
    default_headers: HeaderMap,
    pool: Arc<ImageWorkerPool>,
}

impl HttpTransportClient {
    /// 使用指定 UA 创建带默认图片请求头的客户端。
    fn new(user_agent: &str) -> anyhow::Result<Self> {
        let default_headers = default_image_request_headers(user_agent)?;
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent(user_agent)
            .default_headers(default_headers.clone())
            .connect_timeout(IMAGE_CONNECT_TIMEOUT)
            .timeout(IMAGE_REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            user_agent: HeaderValue::from_str(user_agent)?,
            default_headers,
            pool: Arc::new(ImageWorkerPool::new(IMAGE_WORKER_THREADS)?),
        })
    }

    /// 在阻塞线程上执行一次 HTTP 请求并拼装响应。
    fn execute_request(
        client: reqwest::blocking::Client,
        default_headers: HeaderMap,
        request: http_client::Request<AsyncBody>,
    ) -> anyhow::Result<http_client::Response<AsyncBody>> {
        let (parts, mut body) = request.into_parts();
        let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())?;
        let url = parts.uri.to_string();
        let body_bytes = futures::executor::block_on(async move {
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).await?;
            Ok::<Vec<u8>, io::Error>(bytes)
        })?;

        let mut builder = apply_missing_default_headers(
            client.request(method, url),
            &parts.headers,
            &default_headers,
        );
        if !body_bytes.is_empty() {
            builder = builder.body(body_bytes);
        }

        let response = builder.send()?;
        let status = response.status();
        let version = response.version();
        let headers = response.headers().clone();
        let bytes = read_capped_response(response)?;

        let mut response_builder = http_client::Response::builder()
            .status(status)
            .version(version);
        for (name, value) in &headers {
            response_builder = response_builder.header(name, value);
        }
        Ok(response_builder.body(AsyncBody::from(bytes))?)
    }
}

/// 带上限读取响应体：`Content-Length` 超标直接拒绝，分块传输边读边判。
fn read_capped_response(response: reqwest::blocking::Response) -> anyhow::Result<Vec<u8>> {
    let declared_len = response.content_length();
    read_capped(response, declared_len)
}

/// 实际的上限读取：单独抽出以便用内存缓冲测试，不必起 HTTP 服务。
fn read_capped<R: io::Read>(reader: R, declared_len: Option<u64>) -> anyhow::Result<Vec<u8>> {
    if let Some(declared_len) = declared_len
        && declared_len > MAX_IMAGE_RESPONSE_BYTES
    {
        anyhow::bail!(
            "remote image response is too large: declared {declared_len} bytes, limit {MAX_IMAGE_RESPONSE_BYTES} bytes"
        );
    }

    // 多读 1 字节即可判定「超过上限」，无需先缓冲整份正文。
    let mut bytes = Vec::new();
    reader
        .take(MAX_IMAGE_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMAGE_RESPONSE_BYTES {
        anyhow::bail!(
            "remote image response is too large: exceeded {MAX_IMAGE_RESPONSE_BYTES} bytes"
        );
    }
    Ok(bytes)
}

/// 构造图片请求默认头（含 UA 与缓存控制）。
fn default_image_request_headers(user_agent: &str) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_str(user_agent)?);
    headers.insert(ACCEPT, HeaderValue::from_static(IMAGE_ACCEPT_HEADER));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static(ACCEPT_LANGUAGE_VALUE),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(DEFAULT_CACHE_CONTROL),
    );
    headers.insert(PRAGMA, HeaderValue::from_static(DEFAULT_CACHE_CONTROL));
    Ok(headers)
}

/// 为缺失的请求头补上图片默认头。
fn apply_missing_default_headers(
    builder: reqwest::blocking::RequestBuilder,
    request_headers: &HeaderMap,
    default_headers: &HeaderMap,
) -> reqwest::blocking::RequestBuilder {
    let mut headers = request_headers.clone();
    for (name, value) in default_headers {
        if !headers.contains_key(name) {
            headers.insert(name.clone(), value.clone());
        }
    }
    builder.headers(headers)
}

impl HttpClient for HttpTransportClient {
    /// 返回客户端使用的 UA 头。
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    /// 经工作线程异步发送 HTTP 请求。
    fn send(
        &self,
        request: http_client::Request<AsyncBody>,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<http_client::Response<AsyncBody>>> {
        let client = self.client.clone();
        let default_headers = self.default_headers.clone();
        let pool = self.pool.clone();
        let (tx, rx) = oneshot::channel();
        pool.spawn(Box::new(move || {
            let _ = tx.send(Self::execute_request(client, default_headers, request));
        }));
        async move {
            rx.await
                .map_err(|_| anyhow::anyhow!("image HTTP worker dropped before responding"))?
        }
        .boxed()
    }

    /// 返回代理地址（当前不使用代理）。
    fn proxy(&self) -> Option<&http_client::Url> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ACCEPT_LANGUAGE_VALUE, DEFAULT_CACHE_CONTROL, IMAGE_ACCEPT_HEADER, ImageWorkerPool,
        MAX_IMAGE_RESPONSE_BYTES, apply_missing_default_headers, default_image_request_headers,
        is_remote_image_source, read_capped,
    };
    use reqwest::header::{
        ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, HOST,
        HeaderMap, HeaderValue, PRAGMA, USER_AGENT,
    };
    use std::io::Cursor;
    use std::sync::Arc;

    const RUMARK_USER_AGENT: &str = "RumarkTest/1.0";

    #[test]
    fn default_image_headers_include_browser_like_fetch_context() {
        let headers = default_image_request_headers(RUMARK_USER_AGENT).expect("headers");

        assert_eq!(headers.get(USER_AGENT).unwrap(), RUMARK_USER_AGENT);
        assert_eq!(headers.get(ACCEPT).unwrap(), IMAGE_ACCEPT_HEADER);
        assert_eq!(headers.get(ACCEPT_LANGUAGE).unwrap(), ACCEPT_LANGUAGE_VALUE);
        assert_eq!(headers.get(CACHE_CONTROL).unwrap(), DEFAULT_CACHE_CONTROL);
        assert_eq!(headers.get(PRAGMA).unwrap(), DEFAULT_CACHE_CONTROL);
    }

    #[test]
    fn default_image_headers_leave_transport_managed_headers_unset() {
        let headers = default_image_request_headers(RUMARK_USER_AGENT).expect("headers");

        assert!(!headers.contains_key(ACCEPT_ENCODING));
        assert!(!headers.contains_key(CONNECTION));
        assert!(!headers.contains_key(CONTENT_LENGTH));
        assert!(!headers.contains_key(HOST));
    }

    #[test]
    fn explicit_request_headers_override_default_image_headers() {
        let defaults = default_image_request_headers(RUMARK_USER_AGENT).expect("headers");
        let mut request_headers = HeaderMap::new();
        request_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        request_headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-GB"));

        let request = apply_missing_default_headers(
            reqwest::blocking::Client::new().get("https://example.com/image.png"),
            &request_headers,
            &defaults,
        )
        .build()
        .expect("request should build");
        let headers = request.headers();

        assert_eq!(headers.get(ACCEPT).unwrap(), "application/json");
        assert_eq!(headers.get(ACCEPT_LANGUAGE).unwrap(), "en-GB");
        assert_eq!(headers.get(USER_AGENT).unwrap(), RUMARK_USER_AGENT);
        assert_eq!(headers.get(CACHE_CONTROL).unwrap(), DEFAULT_CACHE_CONTROL);
        assert_eq!(headers.get(PRAGMA).unwrap(), DEFAULT_CACHE_CONTROL);
    }

    #[test]
    fn detects_remote_http_sources() {
        assert!(is_remote_image_source("https://example.com/image.png"));
        assert!(is_remote_image_source("http://example.com/image.gif"));
        assert!(!is_remote_image_source("./image.png"));
        assert!(!is_remote_image_source("images/photo.jpg"));
    }

    #[test]
    fn worker_pool_executes_every_submitted_job() {
        let pool = ImageWorkerPool::new(2).expect("pool should start");
        let (tx, rx) = std::sync::mpsc::channel();
        for index in 0..8 {
            let tx = tx.clone();
            pool.spawn(Box::new(move || {
                let _ = tx.send(index);
            }));
        }
        drop(tx);

        let mut seen: Vec<usize> = rx.iter().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn worker_pool_bounds_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pool = ImageWorkerPool::new(2).expect("pool should start");
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        for _ in 0..12 {
            let active = active.clone();
            let max_active = max_active.clone();
            let done_tx = done_tx.clone();
            pool.spawn(Box::new(move || {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
                let _ = done_tx.send(());
            }));
        }
        drop(done_tx);
        for _ in 0..12 {
            done_rx.recv().expect("job should complete");
        }
        assert!(
            max_active.load(Ordering::SeqCst) <= 2,
            "pool ran more than its worker count concurrently"
        );
    }

    #[test]
    fn response_body_at_the_limit_is_read_in_full() {
        let body = vec![0u8; MAX_IMAGE_RESPONSE_BYTES as usize];

        let read = read_capped(Cursor::new(body), None).expect("at-limit body should be accepted");
        assert_eq!(read.len(), MAX_IMAGE_RESPONSE_BYTES as usize);
    }

    #[test]
    fn oversized_response_body_is_rejected() {
        // 声明长度超标：不必真正读取正文就能拒绝。
        let declared = read_capped(Cursor::new(Vec::new()), Some(MAX_IMAGE_RESPONSE_BYTES + 1));
        let err = declared.expect_err("declared oversize should fail");
        assert!(err.to_string().contains("too large"), "{err}");

        // 分块传输（无 Content-Length）超标同样拒绝。
        let body = vec![0u8; MAX_IMAGE_RESPONSE_BYTES as usize + 1];
        let err = read_capped(Cursor::new(body), None).expect_err("streamed oversize should fail");
        assert!(err.to_string().contains("too large"), "{err}");
    }
}
