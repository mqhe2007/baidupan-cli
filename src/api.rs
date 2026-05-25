use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use reqwest::header::RANGE;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::auth::OAuthClient;
use crate::config::{current_unix_timestamp, AppCredentials, TokenStore, USER_AGENT};
use crate::error::IoContext;
use crate::transfer::UPLOAD_PART_SIZE;
use crate::{Error, Result};

const XPAN_FILE_API: &str = "https://pan.baidu.com/rest/2.0/xpan/file";
const LOCATE_UPLOAD_API: &str = "https://d.pcs.baidu.com/rest/2.0/pcs/file";
const UPLOAD_APP_ID: &str = "250528";
const UPLOAD_VERSION: &str = "2.0";

#[derive(Debug, Clone)]
pub struct PanClient {
    http: Client,
    credentials: AppCredentials,
    token_store: TokenStore,
    oauth: OAuthClient,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteEntry {
    #[serde(default)]
    pub fs_id: u64,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub server_filename: String,
    #[serde(default)]
    pub isdir: i32,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub server_mtime: i64,
    #[serde(default)]
    pub md5: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    list: Vec<RemoteEntry>,
}

#[derive(Debug, Deserialize)]
struct FileMetaResponse {
    info: Vec<FileMetaEntry>,
}

#[derive(Debug, Deserialize)]
struct FileMetaEntry {
    path: String,
    #[serde(deserialize_with = "deserialize_i32_like")]
    isdir: i32,
    size: u64,
    dlink: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrecreateResponse {
    uploadid: Option<String>,
    #[serde(default)]
    block_list: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateResponse {
    path: Option<String>,
    fs_id: Option<u64>,
    size: Option<u64>,
    md5: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadSummary {
    pub path: String,
    pub fs_id: Option<u64>,
    pub size: u64,
    pub md5: Option<String>,
    pub encrypted: bool,
    pub parts: usize,
}

pub struct UploadRequest<'a> {
    pub local_path: &'a Path,
    pub remote_path: &'a str,
    pub size: u64,
    pub block_list: &'a [String],
    pub encrypted: bool,
    pub resume_uploadid: Option<&'a str>,
}

struct UploadPartsRequest<'a> {
    local_path: &'a Path,
    remote_path: &'a str,
    access_token: &'a str,
    uploadid: &'a str,
    upload_host: &'a str,
    total_size: u64,
    total_parts: usize,
    required_parts: &'a [usize],
}

#[derive(Debug, Deserialize)]
struct LocateUploadResponse {
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    error_msg: String,
    #[serde(default)]
    servers: Vec<LocateUploadServer>,
    #[serde(default)]
    bak_servers: Vec<LocateUploadServer>,
}

#[derive(Debug, Deserialize)]
struct LocateUploadServer {
    server: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadSummary {
    pub remote_path: String,
    pub local_path: PathBuf,
    pub size: u64,
    pub decrypted: bool,
}

impl PanClient {
    pub fn new(credentials: AppCredentials, token_store: TokenStore) -> Result<Self> {
        let http = Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self {
            http,
            oauth: OAuthClient::new(&credentials)?,
            credentials,
            token_store,
        })
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<RemoteEntry>> {
        let access_token = self.ensure_access_token().await?;
        let response = self
            .http
            .get(XPAN_FILE_API)
            .query(&[
                ("method", "list"),
                ("dir", normalize_remote_path(path)?.as_str()),
                ("access_token", access_token.as_str()),
            ])
            .send()
            .await?;

        let parsed: ListResponse = parse_pan_success(response, "list").await?;
        Ok(parsed.list)
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let access_token = self.ensure_access_token().await?;
        let response = self
            .http
            .post(XPAN_FILE_API)
            .query(&[
                ("method", "create"),
                ("access_token", access_token.as_str()),
            ])
            .form(&[
                ("path", normalize_remote_path(path)?),
                ("isdir", "1".to_string()),
                ("size", "0".to_string()),
                ("block_list", "[]".to_string()),
            ])
            .send()
            .await?;

        ensure_pan_ok(response, "mkdir").await
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        let payload = json!([normalize_remote_path(path)?]).to_string();
        self.filemanager("delete", payload).await
    }

    pub async fn move_path(&self, from: &str, to: &str) -> Result<()> {
        let payload = move_like_payload(from, to)?;
        self.filemanager("move", payload).await
    }

    pub async fn copy_path(&self, from: &str, to: &str) -> Result<()> {
        let payload = move_like_payload(from, to)?;
        self.filemanager("copy", payload).await
    }

    pub async fn upload_file(
        &self,
        request: UploadRequest<'_>,
        mut on_upload_session: impl FnMut(&str),
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<UploadSummary> {
        let access_token = self.ensure_access_token().await?;
        let remote_path = normalize_remote_path(request.remote_path)?;
        let block_list_json = serde_json::to_string(request.block_list)?;
        let mut precreate_form = vec![
            ("path".to_string(), remote_path.clone()),
            ("size".to_string(), request.size.to_string()),
            ("isdir".to_string(), "0".to_string()),
            ("autoinit".to_string(), "1".to_string()),
            ("rtype".to_string(), "0".to_string()),
            ("block_list".to_string(), block_list_json.clone()),
        ];
        if let Some(uploadid) = request.resume_uploadid.filter(|value| !value.is_empty()) {
            precreate_form.push(("uploadid".to_string(), uploadid.to_string()));
        }

        let precreate = self
            .http
            .post(XPAN_FILE_API)
            .query(&[
                ("method", "precreate"),
                ("access_token", access_token.as_str()),
            ])
            .form(&precreate_form)
            .send()
            .await?;

        let precreate: PrecreateResponse = parse_pan_success(precreate, "precreate").await?;
        let uploadid = precreate
            .uploadid
            .ok_or_else(|| Error::Api("precreate response missing uploadid".to_string()))?;
        on_upload_session(&uploadid);

        let required_parts = if precreate.block_list.is_empty() && !request.block_list.is_empty() {
            (0..request.block_list.len()).collect::<Vec<_>>()
        } else {
            precreate.block_list
        };
        let upload_host = self
            .locate_upload_host(&access_token, &remote_path, &uploadid)
            .await?;

        self.upload_parts(
            UploadPartsRequest {
                local_path: request.local_path,
                remote_path: &remote_path,
                access_token: &access_token,
                uploadid: &uploadid,
                upload_host: &upload_host,
                total_size: request.size,
                total_parts: request.block_list.len(),
                required_parts: &required_parts,
            },
            &mut on_progress,
        )
        .await?;

        let create = self
            .http
            .post(XPAN_FILE_API)
            .query(&[
                ("method", "create"),
                ("access_token", access_token.as_str()),
            ])
            .form(&[
                ("path", remote_path.clone()),
                ("size", request.size.to_string()),
                ("isdir", "0".to_string()),
                ("rtype", "0".to_string()),
                ("uploadid", uploadid),
                ("block_list", block_list_json),
            ])
            .send()
            .await?;

        let created: CreateResponse = parse_pan_success(create, "create").await?;

        Ok(UploadSummary {
            path: created.path.unwrap_or(remote_path),
            fs_id: created.fs_id,
            size: created.size.unwrap_or(request.size),
            md5: created.md5,
            encrypted: request.encrypted,
            parts: request.block_list.len(),
        })
    }

    pub async fn download_file(
        &self,
        remote_path: &str,
        destination: &Path,
        resume_from: u64,
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<DownloadSummary> {
        let access_token = self.ensure_access_token().await?;
        let metadata = self.get_file_metadata_by_path(remote_path, true).await?;

        if metadata.isdir == 1 {
            return Err(Error::Api(format!(
                "{} is a directory; download currently supports files only",
                metadata.path
            )));
        }

        let dlink = metadata
            .dlink
            .clone()
            .ok_or_else(|| Error::Api("download link missing from metadata".to_string()))?;

        if resume_from > metadata.size {
            return Err(Error::Api(format!(
                "local partial file is larger than remote file for {}; remove the partial file and retry",
                metadata.path
            )));
        }

        if metadata.size > 0 && resume_from == metadata.size {
            on_progress(metadata.size, metadata.size);
            return Ok(DownloadSummary {
                remote_path: metadata.path,
                local_path: destination.to_path_buf(),
                size: metadata.size,
                decrypted: false,
            });
        }

        let mut request = self
            .http
            .get(dlink)
            .query(&[("access_token", access_token.as_str())]);

        if resume_from > 0 {
            request = request.header(RANGE, format!("bytes={resume_from}-"));
        }

        let response = ensure_download_ok(request.send().await?).await?;
        let resumed = resume_from > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let initial_offset = if resumed { resume_from } else { 0 };

        let mut file = if resumed {
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(destination)
                .await
                .map_err(|source| Error::Io {
                    path: destination.to_path_buf(),
                    source,
                })?
        } else {
            tokio::fs::File::create(destination)
                .await
                .map_err(|source| Error::Io {
                    path: destination.to_path_buf(),
                    source,
                })?
        };
        let mut stream = response.bytes_stream();
        let mut downloaded = initial_offset;

        on_progress(initial_offset, metadata.size);

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            downloaded += chunk.len() as u64;
            file.write_all(&chunk).await.map_err(|source| Error::Io {
                path: destination.to_path_buf(),
                source,
            })?;
            on_progress(downloaded, metadata.size);
        }

        file.flush().await.map_err(|source| Error::Io {
            path: destination.to_path_buf(),
            source,
        })?;

        Ok(DownloadSummary {
            remote_path: metadata.path,
            local_path: destination.to_path_buf(),
            size: metadata.size,
            decrypted: false,
        })
    }

    async fn filemanager(&self, opera: &str, filelist: String) -> Result<()> {
        let access_token = self.ensure_access_token().await?;
        let response = self
            .http
            .post(XPAN_FILE_API)
            .query(&[
                ("method", "filemanager"),
                ("opera", opera),
                ("async", "0"),
                ("access_token", access_token.as_str()),
            ])
            .form(&[("filelist", filelist)])
            .send()
            .await?;

        ensure_pan_ok(response, "filemanager").await
    }

    async fn ensure_access_token(&self) -> Result<String> {
        let token = self.token_store.load()?;
        if !token.is_expired(current_unix_timestamp()?) {
            return Ok(token.access_token);
        }

        let refreshed = self
            .oauth
            .refresh_token(&self.credentials, &token.refresh_token)
            .await?;
        self.token_store.save(&refreshed)?;
        Ok(refreshed.access_token)
    }

    async fn upload_parts(
        &self,
        request: UploadPartsRequest<'_>,
        on_progress: &mut impl FnMut(u64, u64),
    ) -> Result<()> {
        let mut file = std::fs::File::open(request.local_path).at(request.local_path)?;
        let remaining_bytes = request
            .required_parts
            .iter()
            .try_fold(0_u64, |sum, partseq| {
                let part_len = upload_part_len(request.total_size, request.total_parts, *partseq)?;
                Ok::<u64, Error>(sum + part_len as u64)
            })?;
        let completed_bytes = request.total_size.saturating_sub(remaining_bytes);

        on_progress(completed_bytes, request.total_size);

        let mut transferred = 0_u64;
        for &partseq in request.required_parts {
            let part_len = upload_part_len(request.total_size, request.total_parts, partseq)?;
            let start = partseq as u64 * UPLOAD_PART_SIZE as u64;
            file.seek(SeekFrom::Start(start)).at(request.local_path)?;

            let mut buffer = vec![0_u8; part_len];
            if part_len > 0 {
                file.read_exact(&mut buffer).at(request.local_path)?;
            }

            self.upload_single_part(
                buffer,
                request.upload_host,
                request.remote_path,
                request.access_token,
                request.uploadid,
                partseq,
            )
            .await?;
            transferred += part_len as u64;
            on_progress(completed_bytes + transferred, request.total_size);
        }

        Ok(())
    }

    async fn upload_single_part(
        &self,
        chunk: Vec<u8>,
        upload_host: &str,
        remote_path: &str,
        access_token: &str,
        uploadid: &str,
        partseq: usize,
    ) -> Result<()> {
        let part = Part::bytes(chunk).file_name(format!("part-{partseq}"));
        let form = Form::new().part("file", part);
        let response = self
            .http
            .post(format!("{upload_host}/rest/2.0/pcs/superfile2"))
            .query(&[
                ("method", "upload"),
                ("type", "tmpfile"),
                ("path", remote_path),
                ("uploadid", uploadid),
                ("partseq", &partseq.to_string()),
                ("access_token", access_token),
            ])
            .multipart(form)
            .send()
            .await?;

        ensure_pan_ok(response, "upload").await
    }

    async fn locate_upload_host(
        &self,
        access_token: &str,
        remote_path: &str,
        uploadid: &str,
    ) -> Result<String> {
        let response = self
            .http
            .get(LOCATE_UPLOAD_API)
            .query(&[
                ("method", "locateupload"),
                ("appid", UPLOAD_APP_ID),
                ("access_token", access_token),
                ("path", remote_path),
                ("uploadid", uploadid),
                ("upload_version", UPLOAD_VERSION),
            ])
            .send()
            .await?;

        let payload: LocateUploadResponse = response.json().await?;
        if payload.error_code != 0 {
            return Err(locateupload_payload_error(&payload));
        }

        pick_https_upload_host(&payload)
            .map(str::to_string)
            .ok_or_else(|| Error::Api("locateupload returned no https upload host".to_string()))
    }

    async fn get_file_metadata_by_path(
        &self,
        remote_path: &str,
        dlink: bool,
    ) -> Result<FileMetaEntry> {
        let remote_path = normalize_remote_path(remote_path)?;
        let entry = self.resolve_entry_by_path(&remote_path).await?;
        let access_token = self.ensure_access_token().await?;
        let fsids = serde_json::to_string(&vec![entry.fs_id])?;
        let response = self
            .http
            .get(XPAN_FILE_API)
            .query(&[
                ("method", "filemetas"),
                ("access_token", access_token.as_str()),
                ("fsids", fsids.as_str()),
                ("dlink", if dlink { "1" } else { "0" }),
            ])
            .send()
            .await?;

        let metadata: FileMetaResponse = parse_pan_success(response, "filemetas").await?;
        metadata
            .info
            .into_iter()
            .next()
            .ok_or_else(|| Error::Api(format!("no metadata returned for {}", remote_path)))
    }

    async fn resolve_entry_by_path(&self, remote_path: &str) -> Result<RemoteEntry> {
        let parent = parent_remote_dir(remote_path);
        let name = Path::new(remote_path)
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| Error::InvalidRemotePath(remote_path.to_string()))?;

        let entries = self.list_dir(&parent).await?;
        entries
            .into_iter()
            .find(|entry| entry.path == remote_path || entry.server_filename == name)
            .ok_or_else(|| Error::Api(format!("remote path not found: {}", remote_path)))
    }
}

async fn ensure_pan_ok(response: reqwest::Response, operation: &str) -> Result<()> {
    let payload: Value = response.json().await?;
    let errno = payload.get("errno").and_then(Value::as_i64).unwrap_or(0);
    if errno == 0 {
        return Ok(());
    }
    Err(api_payload_error(operation, payload))
}

async fn parse_pan_success<T>(response: reqwest::Response, operation: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let payload: Value = response.json().await?;
    if let Some(errno) = payload.get("errno").and_then(Value::as_i64) {
        if errno != 0 {
            return Err(api_payload_error(operation, payload));
        }
    }
    Ok(serde_json::from_value(payload)?)
}

async fn ensure_download_ok(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let body = response.text().await?;
    Err(download_payload_error(status, &body))
}

fn api_payload_error(operation: &str, payload: Value) -> Error {
    let errno = payload
        .get("errno")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let extra = payload
        .get("errmsg")
        .or_else(|| payload.get("error_msg"))
        .and_then(Value::as_str)
        .map(|value| format!(", message={value}"))
        .unwrap_or_default();
    let hint = openapi_errno_hint(errno)
        .map(|value| format!(", hint={value}"))
        .unwrap_or_default();

    Error::Api(format!(
        "{operation} failed with errno={errno}{hint}{extra}: {payload}"
    ))
}

fn download_payload_error(status: reqwest::StatusCode, body: &str) -> Error {
    if let Ok(payload) = serde_json::from_str::<Value>(body) {
        if let Some(code) = payload
            .get("errno")
            .or_else(|| payload.get("error_code"))
            .and_then(Value::as_i64)
        {
            let extra = payload
                .get("errmsg")
                .or_else(|| payload.get("error_msg"))
                .and_then(Value::as_str)
                .map(|value| format!(", message={value}"))
                .unwrap_or_default();
            let hint = openapi_errno_hint(code)
                .map(|value| format!(", hint={value}"))
                .unwrap_or_default();

            return Error::Api(format!(
                "download failed with status={} errno={}{}{}: {}",
                status, code, hint, extra, payload
            ));
        }
    }

    Error::Api(format!(
        "download failed with status={status}: {}",
        body.trim()
    ))
}

fn locateupload_payload_error(payload: &LocateUploadResponse) -> Error {
    let hint = openapi_errno_hint(payload.error_code)
        .map(|value| format!(", hint={value}"))
        .unwrap_or_default();
    let extra = if payload.error_msg.is_empty() {
        String::new()
    } else {
        format!(", message={}", payload.error_msg)
    };

    Error::Api(format!(
        "locateupload failed with error_code={}{}{}",
        payload.error_code, hint, extra
    ))
}

fn openapi_errno_hint(errno: i64) -> Option<&'static str> {
    match errno {
        -1 => Some("当前权益已过期；请确认账号状态后重试"),
        -3 => Some("文件不存在；请确认远端路径或文件标识是否正确"),
        -6 => Some("身份验证失败；请检查 access_token 是否有效，并确认授权成功"),
        -10 => Some("网盘容量不足；请清理空间后重试上传"),
        -8 => Some("远端已存在同名文件；请更换路径，或后续支持覆盖/重命名策略后再试"),
        -9 => Some("文件或目录不存在；请确认路径正确，或目标尚未被删除/移动"),
        -7 => Some("文件名或路径不合法，或者当前授权无权访问该路径；请检查远端路径和应用权限"),
        1 => Some("百度开放平台返回未知错误；如果频繁出现，建议稍后重试或联系平台支持"),
        2 => Some("参数错误；请检查必填参数、参数位置以及参数值是否合法"),
        3 => Some("接口方法不受支持；请检查请求 method 和 API 路径是否正确"),
        4 | 18 => Some("接口 QPS 达到上限；请降低并发或稍后重试"),
        5 => Some("当前客户端 IP 不在白名单内；请检查开放平台安全设置"),
        6 => Some("当前应用不允许接入用户数据；建议稍后重新授权，或检查应用审核状态"),
        17 => Some("应用已达到每日访问限额；请等待配额恢复或申请更高配额"),
        19 => Some("应用已达到总调用量限额；请检查控制台配额设置"),
        100 => Some("请求缺少 access_token 或参数不合法；请确认已登录且请求参数完整"),
        110 => Some("access token 无效；请重新登录以获取新的 token"),
        111 => Some("有其他异步任务正在执行；请稍后重试当前操作"),
        20011 => Some("应用仍在审核中；未上线前仅限前 10 个授权用户测试"),
        20012 => Some("应用访问超限；请检查应用审核状态与调用频率"),
        20013 => Some("当前应用缺少接口权限；请检查审核状态和已申请能力"),
        213 => Some("当前授权没有访问用户手机号的权限；请调整应用权限范围"),
        10 => Some("创建文件失败；通常是分片、文件大小或 block_list 与预上传阶段不一致"),
        31023 => Some("参数错误；请检查必选参数、URL 参数和表单参数是否都正确"),
        31024 => Some("应用没有上传权限；请先在百度网盘开放平台申请开通上传能力"),
        31034 => Some("命中接口频控；请降低请求频率后重试"),
        31045 => Some("access_token 验证未通过；请检查 token 是否过期，并确认授权包含网盘权限"),
        31061 => Some("文件已存在；请更换目标路径或调整重复文件处理策略"),
        31062 => Some("文件名无效；请检查是否包含不允许的特殊字符"),
        31064 => Some("上传路径错误；网盘开放平台上传路径必须符合 /apps/应用名/... 的限制"),
        31066 => Some("文件名不存在；请检查路径是否正确，以及目标文件是否确实存在"),
        31190 => {
            Some("云端没有找到待创建文件；通常是分片上传不完整、block_list 不匹配或 size 不一致")
        }
        31299 => Some("第一个分片小于 4MB；普通用户分片大小应固定为 4MB，最后一个分片除外"),
        31326 => Some("命中防盗链；请确认下载请求头里带了 User-Agent: pan.baidu.com"),
        31355 => Some(
            "uploadid 参数异常；请确认 create 或续传时使用的是 precreate 返回的同一个 uploadid",
        ),
        31360 => Some("链接已过期；请重新获取下载链接或重新发起上传定位请求"),
        31362 => Some("签名错误；请检查下载链接是否完整，以及 access_token 是否正确拼接"),
        31363 => {
            Some("分片缺失；请检查是否所有分片都已上传，以及 size、partseq、block_list 是否一致")
        }
        31364 => Some("分片大小超限；普通用户建议固定使用 4MB 分片"),
        31365 => Some("文件总大小超出当前账号等级限制；普通用户上限 4GB，会员与超级会员更高"),
        _ => None,
    }
}

fn upload_part_len(total_size: u64, total_parts: usize, partseq: usize) -> Result<usize> {
    if partseq >= total_parts {
        return Err(Error::Api(format!(
            "precreate returned unexpected part index {partseq}, total parts={total_parts}"
        )));
    }

    let start = partseq as u64 * UPLOAD_PART_SIZE as u64;
    if start > total_size {
        return Err(Error::Api(format!(
            "computed upload offset {start} exceeds file size {total_size}"
        )));
    }

    Ok(total_size
        .saturating_sub(start)
        .min(UPLOAD_PART_SIZE as u64) as usize)
}

fn pick_https_upload_host(payload: &LocateUploadResponse) -> Option<&str> {
    payload
        .servers
        .iter()
        .chain(payload.bak_servers.iter())
        .map(|server| server.server.trim())
        .find(|server| server.starts_with("https://"))
}

fn deserialize_i32_like<'de, D>(deserializer: D) -> std::result::Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntLike {
        Int(i32),
        Text(String),
    }

    match IntLike::deserialize(deserializer)? {
        IntLike::Int(value) => Ok(value),
        IntLike::Text(value) => value.parse::<i32>().map_err(serde::de::Error::custom),
    }
}

fn normalize_remote_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidRemotePath(path.to_string()));
    }
    if trimmed == "/" {
        return Ok("/".to_string());
    }

    let normalized = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };

    if normalized.contains("//") {
        return Err(Error::InvalidRemotePath(path.to_string()));
    }
    Ok(normalized)
}

fn move_like_payload(from: &str, to: &str) -> Result<String> {
    let from = normalize_remote_path(from)?;
    let to = normalize_remote_path(to)?;
    let destination = Path::new(&to);
    let parent = destination
        .parent()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("/");
    let newname = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::InvalidRemotePath(to.clone()))?;

    Ok(json!([
        {
            "path": from,
            "dest": parent,
            "newname": newname,
        }
    ])
    .to_string())
}

fn parent_remote_dir(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }

    Path::new(path)
        .parent()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("/")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_remote_paths() {
        assert_eq!(
            normalize_remote_path("docs/file.txt").expect("normalize"),
            "/docs/file.txt"
        );
        assert_eq!(normalize_remote_path("/").expect("root"), "/");
    }

    #[test]
    fn builds_move_payload() {
        let payload = move_like_payload("/from/a.txt", "/to/b.txt").expect("payload");
        let parsed: Value = serde_json::from_str(&payload).expect("json");

        assert_eq!(parsed[0]["path"], "/from/a.txt");
        assert_eq!(parsed[0]["dest"], "/to");
        assert_eq!(parsed[0]["newname"], "b.txt");
    }

    #[test]
    fn derives_parent_remote_dir() {
        assert_eq!(parent_remote_dir("/docs/file.txt"), "/docs");
        assert_eq!(parent_remote_dir("/file.txt"), "/");
    }

    #[test]
    fn maps_known_openapi_errno_to_hint() {
        let error = api_payload_error("list", json!({"errno": 31045}));

        assert!(error.to_string().contains("access_token 验证未通过"));
        assert!(error.to_string().contains("errno=31045"));
    }

    #[test]
    fn keeps_unknown_errno_without_hint() {
        let error = api_payload_error("list", json!({"errno": 99999, "errmsg": "mystery"}));

        assert!(error.to_string().contains("errno=99999"));
        assert!(error.to_string().contains("message=mystery"));
        assert!(!error.to_string().contains("hint="));
    }

    #[test]
    fn maps_upload_chunk_missing_errno_to_hint() {
        let error = api_payload_error("request", json!({"errno": 31363}));

        assert!(error.to_string().contains("分片缺失"));
        assert!(error.to_string().contains("partseq"));
    }

    #[test]
    fn maps_uploadid_errno_to_hint() {
        let error = api_payload_error("create", json!({"errno": 31355}));

        assert!(error.to_string().contains("uploadid 参数异常"));
        assert!(error.to_string().contains("同一个 uploadid"));
    }

    #[test]
    fn maps_locateupload_error_code_to_hint() {
        let error = locateupload_payload_error(&LocateUploadResponse {
            error_code: 4,
            error_msg: "qps limited".to_string(),
            servers: Vec::new(),
            bak_servers: Vec::new(),
        });

        assert!(error.to_string().contains("error_code=4"));
        assert!(error.to_string().contains("QPS 达到上限"));
        assert!(error.to_string().contains("message=qps limited"));
    }

    #[test]
    fn maps_async_task_conflict_errno_to_hint() {
        let error = api_payload_error("filemanager", json!({"errno": 111}));

        assert!(error.to_string().contains("异步任务"));
        assert!(!error.to_string().contains("token 已过期"));
    }

    #[test]
    fn maps_download_signature_errno_to_hint() {
        let error = download_payload_error(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error_code":31362,"error_msg":"bad sign"}"#,
        );

        assert!(error.to_string().contains("签名错误"));
        assert!(error.to_string().contains("status=403"));
    }

    #[test]
    fn maps_download_antileech_errno_to_hint() {
        let error =
            download_payload_error(reqwest::StatusCode::FORBIDDEN, r#"{"error_code":31326}"#);

        assert!(error.to_string().contains("User-Agent: pan.baidu.com"));
    }

    #[test]
    fn parses_filemetas_info_payload() {
        let payload = json!({
            "errno": 0,
            "info": [
                {
                    "path": "/apps/demo/a.txt",
                    "isdir": "0",
                    "size": 17,
                    "dlink": "https://example.com/file"
                }
            ]
        });

        let parsed: FileMetaResponse = serde_json::from_value(payload).expect("parse filemetas");

        assert_eq!(parsed.info.len(), 1);
        assert_eq!(parsed.info[0].path, "/apps/demo/a.txt");
        assert_eq!(
            parsed.info[0].dlink.as_deref(),
            Some("https://example.com/file")
        );
    }

    #[test]
    fn prefers_https_upload_host_from_locateupload() {
        let payload = LocateUploadResponse {
            error_code: 0,
            error_msg: String::new(),
            servers: vec![
                LocateUploadServer {
                    server: "http://c3.pcs.baidu.com".to_string(),
                },
                LocateUploadServer {
                    server: "https://c3.pcs.baidu.com".to_string(),
                },
            ],
            bak_servers: vec![LocateUploadServer {
                server: "https://c.pcs.baidu.com".to_string(),
            }],
        };

        assert_eq!(
            pick_https_upload_host(&payload),
            Some("https://c3.pcs.baidu.com")
        );
    }

    #[test]
    fn falls_back_to_backup_https_upload_host() {
        let payload = LocateUploadResponse {
            error_code: 0,
            error_msg: String::new(),
            servers: vec![LocateUploadServer {
                server: "http://c2.pcs.baidu.com".to_string(),
            }],
            bak_servers: vec![LocateUploadServer {
                server: "https://c.pcs.baidu.com".to_string(),
            }],
        };

        assert_eq!(
            pick_https_upload_host(&payload),
            Some("https://c.pcs.baidu.com")
        );
    }

    #[test]
    fn computes_upload_part_lengths() {
        assert_eq!(upload_part_len(10, 1, 0).expect("part 0"), 10);
        assert_eq!(
            upload_part_len((UPLOAD_PART_SIZE as u64) + 2, 2, 1).expect("part 1"),
            2
        );
    }
}
