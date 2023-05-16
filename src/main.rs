/*
# rust-download-subtitles
Download subtitles for TV shows from OpenSubtitles.com

Copyright © 2022 Fabio A. Correa Duran facorread@gmail.com
    
Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

  https://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

mod rust_download_subtitles {
    use serde_derive::{Deserialize, Serialize};
    
    #[derive(Clone, Copy, Debug)]
    pub struct TaskIndex {
        pub task_index: usize,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct TokenIndex {
        pub token_index: usize, // 0 is for unauthenticated API use, such as /subtitles
    }

    pub static TOKEN_0: TokenIndex = TokenIndex {token_index: 0};

    // cf UnboundedSender
    type HttpSender = tokio::sync::mpsc::Sender<(TaskIndex, TokenIndex, reqwest::Request)>;
    type Receiver = tokio::sync::mpsc::Receiver<(TaskIndex, TokenIndex, reqwest::Request)>;

    // cf UnboundedSender
    pub type ResponseSender = tokio::sync::mpsc::Sender<(TokenIndex, reqwest::Response)>;
    type ResponseChannel = tokio::sync::mpsc::Receiver<(TokenIndex, reqwest::Response)>;

    pub trait HttpStatus {
        fn check_status(&self) -> anyhow::Result<()>;
        fn try_again_later(&self) -> bool;
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct ClientConfig {
        pub api_key: String,
        pub account: Vec<(String, String)>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct EpisodeConfig {
        pub destination_folder: String,
        pub episode_from: u32,
        pub episode_to: u32, // Inclusive
        pub languages: String,
        pub latest_only: bool, // true to download only the latest subtitle for the episode; false to download all subtitles
        pub parent_imdb_id: String, // String because it is not involved in padding or operations
        pub season_number: u32, // This will be padded with zeroes
        pub show_year: String, // String because it is not involved in padding or operations
        pub title: String, // Name of the TV show
    }

    #[derive(Debug)]
    pub struct EpisodeSeq {
        pub size: usize,
        pub range: std::ops::Range<u32>,
    }

    impl EpisodeConfig {
        pub fn episode_seq(&self) -> anyhow::Result<EpisodeSeq> {
            let episode_end = self.episode_to + 1;
            if episode_end > self.episode_from {
                use anyhow::Context;
                Ok(EpisodeSeq {
                    size: usize::try_from(episode_end - self.episode_from).context("Internal operation on EpisodeConfig")?,
                    range: self.episode_from..episode_end,
                })
            } else {
                Ok(EpisodeSeq {
                    size: 1,
                    range: self.episode_from..(self.episode_from + 1),
                })
            }
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct Config {
        pub client_config: ClientConfig,
        pub episode_config: EpisodeConfig,
    }

    #[derive(Debug, Serialize)]
    pub struct LoginRequestBody {
        username: String,
        password: String
    }

    impl LoginRequestBody {
        pub fn new(username: &str, password: &str) -> Self {
            Self {
                username: String::from(username),
                password: String::from(password),
            }
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct LoginResponse {
        token: String,
        // pub status: u16, // The HTTP status code is already handled through HttpStatus.
    }

    impl LoginResponse {
        pub fn token(&self) -> anyhow::Result<reqwest::header::HeaderValue> {
            use anyhow::Context;
            reqwest::header::HeaderValue::try_from(format!("Bearer {}", self.token)).context("Processing login session token")
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct FileMatchResponse {
        file_id: u32,
    }

    #[derive(Debug, Deserialize)]
    pub struct SubtitleMatchAttributesResponse {
        // subtitle_id: String,
        hearing_impaired: bool,
        // upload_date: String,
        legacy_subtitle_id: u32,
        files: Vec<FileMatchResponse>,
    }

    #[derive(Debug, Deserialize)]
    pub struct SubtitleMatchResponse {
        // id: String,
        // #[serde(rename = "type")] 
        // sub_type: String,
        attributes: SubtitleMatchAttributesResponse,
    }

    fn sub_compare<'r, 's>(sub: &'r &SubtitleMatchResponse, other_sub: &'s &SubtitleMatchResponse) -> std::cmp::Ordering {
        let hearing_cmp = sub.attributes.hearing_impaired.cmp(&other_sub.attributes.hearing_impaired);
        if hearing_cmp == std::cmp::Ordering::Equal {
            sub.attributes.legacy_subtitle_id.cmp(&other_sub.attributes.legacy_subtitle_id)
        } else {
            hearing_cmp
        }
        // Note: enum Ordering implements Ord itself, which facilitates compositing
        // two cmp() calls into a total ordering. But the effect is different
        // to that of this function! Composition is not what we want. Just for reference,
        // find "impl Ord for Ordering" at https://doc.rust-lang.org/stable/src/core/cmp.rs.html
    }

    #[derive(Debug, Deserialize)]
    pub struct SearchResultsResponse {
        // total_count: u32,
        data: Vec<SubtitleMatchResponse>,
    }

    impl SearchResultsResponse {
        /// Returns the file ids for the best subtitles
        pub fn file_ids(&self, episode_number: u32, latest_only: bool) -> anyhow::Result<Vec<u32>> {
            let iter = self.data.iter().filter(|s| !s.attributes.files.is_empty());
            if latest_only {
                if let Some(best_sub) = iter.clone().max_by(sub_compare) {
                    return Ok(best_sub.attributes.files.iter().map(|s| s.file_id).collect())
                }
            }
            let mut res_vec: Vec<u32> = Vec::with_capacity(2 * self.data.len());
            for sub in iter {
                for file in &sub.attributes.files {
                    res_vec.push(file.file_id);
                }
            }
            if res_vec.is_empty() {
                use anyhow::bail;
                bail!(format!("No subtitles found for episode {}", episode_number))
            } else {
                Ok(res_vec)
            }
        }
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct DownloadResponse {
        link: String,
        file_name: String,
        requests: u32,
        remaining: u32,
        message: String,
        reset_time: String,
    }

    impl DownloadResponse {
        pub fn link(&self) -> anyhow::Result<(reqwest::Url, String)> {
            use anyhow::Context;
            use std::str::FromStr;
            let url = || reqwest::Url::from_str(&self.link).context("Preparing the download");
            if let Some((_, file_extension)) = self.link.rsplit_once('.') {
                Ok((url()?, String::from(file_extension)))
            } else {
                Ok((url()?, String::from(".srt")))
            }
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct MessageResponse {
        pub message: String,
        // pub status: u16, // The HTTP status code is already handled through HttpStatus.
    }

    impl std::fmt::Display for MessageResponse {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl HttpStatus for reqwest::StatusCode {
        fn check_status(&self) -> anyhow::Result<()> {
            anyhow::ensure!(!self.is_client_error(), format!("OpenSubtitles returned a client error: {}", self.canonical_reason().unwrap_or("No reason given")));
            Ok(())
        }

        fn try_again_later(&self) -> bool {
            self.is_server_error() || *self == reqwest::StatusCode::TOO_MANY_REQUESTS
        }
    }

    pub fn client(headers: reqwest::header::HeaderMap) -> reqwest::Result<reqwest::Client> {
        use std::time::Duration;
        // let interval = Duration::new(30, 0);
        let timeout = Duration::new(10, 0);
        // .http2_max_frame_size(Some(10000000)).http2_prior_knowledge() // Forced http/2 does not work
        // .http2_keep_alive_interval(interval).http2_keep_alive_timeout(timeout).http2_keep_alive_while_idle(true)
        reqwest::Client::builder().connect_timeout(timeout).connection_verbose(true).default_headers(headers).build()
    }
   
    async fn create_file_impl(mut path: std::path::PathBuf, extension: &str) -> anyhow::Result<tokio::fs::File>
    {
        use anyhow::{ensure,Context};
        path.set_extension(extension);
        let file = tokio::fs::OpenOptions::new().create(true).write(true).open(&path).await.context(format!("Creating file {}", path.display()))?;
        ensure!(file.metadata().await?.len() == 0, format!("{}: File is not empty", path.display()));
        Ok(file)
    }

    pub async fn create_file(dir_name: &std::path::PathBuf, mut file_name: String, extension: &str) -> anyhow::Result<tokio::fs::File> {
        // anyhow::Context is not used because the context messages would remain unused.
        // The only possible error message to be displayed is at the end of this function.
        // or_else() cannot be used because async closures are unstable
        // https://doc.rust-lang.org/error-index.html#E0708
        // https://github.com/rust-lang/rust/issues/62290
        let mut dir_name = dir_name.clone();
        dir_name.push(&file_name);
        tokio::fs::create_dir_all(&dir_name).await?;
        {
            let mut full_path = dir_name.clone();
            full_path.push(&file_name);
            let file_result = create_file_impl(full_path, extension).await;
            if file_result.is_ok() {
                return file_result;
            }
        }
        file_name.push('_');
        let number_of_subtitles = 100;
        for serial_number in 1..number_of_subtitles {
            let mut file_name = file_name.clone();
            file_name.push_str(&serial_number.to_string());
            let mut full_path = dir_name.clone();
            full_path.push(&file_name);
            let file_result = create_file_impl(full_path, extension).await;
            if file_result.is_ok() {
                return file_result;
            }
        }
        anyhow::bail!("Giving up after {} tries to create output files. Is there any problem with the output volume?", number_of_subtitles)
    }

    pub fn request_channel(buffer: usize) -> (HttpSender, Receiver) {
        // cf unbounded_channel()
        tokio::sync::mpsc::channel(buffer)
    }

    pub fn response_channel() -> (ResponseSender, ResponseChannel) {
        // cf unbounded_channel()
        tokio::sync::mpsc::channel(1)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Section 1: Load configuration and check that the local resources,
    // namely the destination folder, are available.
    use anyhow::Context;
    use ron::de::from_str;
    use std::str::FromStr;
    use tokio::io::AsyncReadExt;
    // flexi_logger::Logger::try_with_str("Debug")?.start()?;
    let mut config_file = tokio::fs::OpenOptions::new().read(true).open("config.ron").await.context("Opening configuration file config.ron (see documentation)")?;
    let mut buf = Vec::new();
    config_file.read_to_end(&mut buf).await?;
    let config = from_str::<rust_download_subtitles::Config>(&String::from_utf8(buf).context("Checking that config.ron is valid text")?).context("Checking that config.ron contains valid variables")?;
    // println!("Configuration file was read correctly. Exiting. {:?}", config); return Ok(());
    let episode_config = config.episode_config;
    let dir_name = std::path::PathBuf::from_str(&episode_config.destination_folder).with_context(|| format!("Checking that the destination folder name is valid text: {}", episode_config.destination_folder))?;
    {
        let destination_folder_exists = dir_name.try_exists().with_context(|| format!("Checking that volume is mounted and destination folder exists: {}", episode_config.destination_folder));
        anyhow::ensure!(destination_folder_exists?, format!("The destination folder {} does not exist. Is the volume mounted?", episode_config.destination_folder));
        println!("Destination folder {}", episode_config.destination_folder);
    }
    let url = reqwest::Url::parse("https://api.opensubtitles.com/api/v1/")?;
    let episode_seq = episode_config.episode_seq()?;
    let (http_sender, mut receiver) = rust_download_subtitles::request_channel(episode_seq.size);
    let mut responses_vec = Vec::with_capacity(episode_seq.size);
    let mut join_set = tokio::task::JoinSet::<anyhow::Result<()>>::new();
    // Section 2: One task for each episode
    for (task_index, episode) in episode_seq.range.enumerate() {
        let task_index = rust_download_subtitles::TaskIndex{task_index};
        let url = url.clone();
        let dir_name = dir_name.clone();
        let http_sender = http_sender.clone();
        let http_sender_end = http_sender.clone();
        let episode_config = episode_config.clone();
        let (response_sender, mut response_channel) = rust_download_subtitles::response_channel();
        responses_vec.push(response_sender);
        join_set.spawn(async move {
            use anyhow::ensure;
            use lazy_format::lazy_format;
            use reqwest::{Request, Method, Url};
            let mut token_index = rust_download_subtitles::TOKEN_0;
            let subtitle_handle = tokio::spawn(async move {
                let file_ids = {
                    use rust_download_subtitles::SearchResultsResponse;
                    // &hearing_impaired=include // This parameter leads to an unnecessary redirect
                    let url = url.join(&format!("subtitles?episode_number={episode}&foreign_parts_only=exclude&languages={}&parent_imdb_id={}&season_number={}", episode_config.languages, episode_config.parent_imdb_id, episode_config.season_number)).context("Creating the /subtitles url")?;
                    let request = Request::new(Method::GET, url);
                    http_sender.try_send((task_index, token_index, request)).context("Preparing a request for /subtitles")?;
                    let (tok_index, response) = response_channel.recv().await.context(lazy_format!("Processing a response from /subtitles for episode {episode}"))?;
                    token_index = tok_index;
                    ensure!(response.status().is_success(), lazy_format!("{} for /subtitles for episode {episode}", response.status()));
                    let search_results = response.json::<SearchResultsResponse>().await.context(lazy_format!("Processing the list of subtitles for episode {episode}"))?;
                    search_results.file_ids(episode, episode_config.latest_only).context(lazy_format!("Enumerating subtitles for episode {episode}"))?
                };
                for file_id in file_ids {
                    let (url, file_extension) = {
                        use reqwest::Body;
                        use rust_download_subtitles::DownloadResponse;
                        let body = Some(Body::from(format!("{{\"file_id\": {file_id}}}")));
                        let url = url.join("download").context("Creating the /download url")?;
                        let mut request = Request::new(Method::POST, url);
                        *request.body_mut() = body;
                        http_sender.try_send((task_index, token_index, request)).context(lazy_format!("Preparing a request for /download for episode {episode} and file id {file_id}"))?;
                        let (tok_index, response) = response_channel.recv().await.context(lazy_format!("Receiving a response from /download for episode {episode} and file id {file_id}"))?;
                        token_index = tok_index;
                        ensure!(response.status().is_success(), lazy_format!("{} for /download for episode {episode}", response.status()));
                        let dl_response = response.json::<DownloadResponse>().await.context(lazy_format!("Processing a response from /download for episode {episode} and file id {file_id}"))?;
                        let (url, file_extension) = dl_response.link()?;
                        (url, file_extension)
                    };
                    use std::io::Cursor;
                    use tokio::io::AsyncWriteExt;
                    let request = Request::new(Method::GET, url);
                    http_sender.try_send((task_index, token_index, request)).context(lazy_format!("Preparing a request for a temporary download link for episode {episode} and file id {file_id}"))?;
                    let (tok_index, response) = response_channel.recv().await.context(lazy_format!("Receiving a response for a temporary download link for episode {episode} and file id {file_id}"))?;
                    token_index = tok_index;
                    ensure!(response.status().is_success(), lazy_format!("{} for a temporary download link for episode {episode} and file id {file_id}", response.status()));
                    let mut buf = Cursor::new(response.text().await.context(lazy_format!("Processing response for a temporary download link for episode {episode} and file id {file_id}"))?);
                    // let file_name = format!("{}{:02}{}{:02}", &episode_config.show_year, episode, &episode_config.title, &episode_config.season_number);
                    let file_name = format!("{} S{:02}E{:02} {}", &episode_config.title, &episode_config.season_number, episode, &episode_config.show_year);
                    let mut srt_file = rust_download_subtitles::create_file(&dir_name, file_name, &file_extension).await.context(lazy_format!("Creating subtitle file for episode {episode} and file id {file_id}"))?;
                    srt_file.write_all_buf(&mut buf).await.context(lazy_format!("Writing subtitle file for episode {episode} and file id {file_id}"))?;
                }
                print!(" {episode}");
                Ok::<(), anyhow::Error>(())
            });
            let subtitle_result = subtitle_handle.await;
            let request = Request::new(Method::DELETE, Url::from_str("https://example.com")?); // url will remain unused. This is NOT sent to OpenSubtitles.
            http_sender_end.try_send((task_index, token_index, request)).context(lazy_format!("Wrapping up subtitle downloading for episode {episode}"))?; // This is NOT sent to OpenSubtitles.
            subtitle_result.context(lazy_format!("Finishing a subtitle task for episode {episode}"))?.context(lazy_format!("After finishing a subtitle task for episode {episode}"))?;
            Ok(())
        });
    }
    // Section 3: HTTP client
    // This section communicates with the OpenSubtitles server,
    // following its mandatory rate limitations.
    let mut client = Vec::with_capacity(1 + config.client_config.account.len());
    {
        use lazy_format::lazy_format;
        use reqwest::header::HeaderValue;
        let mut headers = reqwest::header::HeaderMap::with_capacity(20);
        {
            let mut api_key = HeaderValue::from_str(&config.client_config.api_key)?;
            api_key.set_sensitive(true);
            headers.insert("Api-Key", api_key);
        }
        headers.insert(reqwest::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let login_client = rust_download_subtitles::client(headers.clone()).context(lazy_format!("Creating an HTTP client for /login"))?;
        let mut login_next_instant = tokio::time::Instant::now();
        for (login, password) in config.client_config.account.into_iter() {
            // Section 3.1: Login
            // We don't use iterators to build the client vector, because login might fail.
            let login_result = async {
                use rust_download_subtitles::LoginRequestBody;
                use tokio::time::{sleep_until, Instant, Duration};
                let body = LoginRequestBody::new(&login, &password);
                let url = url.join("login").context("Creating a /login url")?;
                let repeat_request = login_client.post(url).json(&body);
                loop {
                    use reqwest::header::AUTHORIZATION;
                    use rust_download_subtitles::{HttpStatus, LoginResponse};
                    sleep_until(login_next_instant).await;
                    let response = repeat_request.try_clone().context("Preparing a login request")?.send().await.context("Sending a login request")?;
                    login_next_instant = Instant::now() + Duration::from_secs(1);
                    let status = response.status();
                    if status.try_again_later() {
                        continue; // Will try again in a second
                    }
                    if status.is_success() {
                        let login_response = response.json::<LoginResponse>().await.context(lazy_format!("Processing the login response for account {}", &login))?;
                        let mut headers = headers.clone();
                        headers.insert(AUTHORIZATION, login_response.token().context("Processing a session token")?);
                        client.push(rust_download_subtitles::client(headers).context("Creating an HTTP client")?);
                        return Ok::<(), anyhow::Error>(());
                    } else {
                        use rust_download_subtitles::MessageResponse;
                        use serde_json::from_str;
                        let text = response.text().await.context(lazy_format!("{status} when processing the login response for account {}", &login))?;
                        let response = from_str::<MessageResponse>(&text).context(lazy_format!("{status} when processing a login response: {text}"))?;
                        anyhow::bail!("{status} response on a /login request: {response}");
                    }
                }
            }.await;
            if let Err(e) = login_result {
                println!("Login failed: {:?}", e)
            }
        }
    }
    anyhow::ensure!(!client.is_empty(), "Fatal error: login failed.");
    let mut _next_instant = tokio::time::Instant::now();
    print!("Downloading episode:");
    // Section 3.2 Processing requests
    let processing_result = async {
        let mut number_finished_subtitles = 0;
        while number_finished_subtitles < episode_seq.size {
            use tokio::time::{sleep_until, Instant, Duration};
            let (task_index, mut token_index, request) = receiver.recv().await.context("Processing an HTTP request")?;
            if request.method() == reqwest::Method::DELETE {
                number_finished_subtitles += 1;
                continue;
            }
            let token_orig = token_index;
            loop {
                use lazy_format::lazy_format;
                use rust_download_subtitles::HttpStatus;
                sleep_until(_next_instant).await;
                let response = client.get(token_index.token_index).context(lazy_format!("Internal error on client.get(token_index)"))?.execute(request.try_clone().context(lazy_format!("Internal error on request.try_clone()"))?).await.context("Receiving an HTTP response")?;
                let status = response.status();
                if status.try_again_later() {
                    _next_instant = Instant::now() + Duration::from_secs(1);
                    continue; // Will try again in a second
                }
                _next_instant = Instant::now() + Duration::from_millis(200);
                let status = response.status();
                if status.is_client_error() {
                    // Most client errors are ignored in this section
                    let idx = &mut token_index.token_index;
                    *idx += 1;
                    if *idx == client.len() {
                        *idx = 0;
                    }
                    if token_index != token_orig {
                        continue; // Keep trying
                    }
                    // if token_index == token_orig then continue below: the client error is sent to the task
                    // The calling task will fail, sending a DELETE request to this task
                }
                responses_vec.get_mut(task_index.task_index).context("Connecting to an async task")?.try_send((token_index, response)).context("Sending a response to an async task")?;
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    }.await;
    if let Err(e) = processing_result {
        println!("Error processing HTTP requests: {:?}", e)
    }
    // Section 3.3: Logout
    {
        let url = url.join("logout").context("Creating the /logout url")?;
        for cli in client.into_iter() {
            use lazy_format::lazy_format;
            use rust_download_subtitles::{HttpStatus, MessageResponse};
            use tokio::time::{sleep_until, Instant, Duration};
            sleep_until(_next_instant).await;
            let response = cli.delete(url.clone()).send().await?;
            let status = response.status();
            let response = response.json::<MessageResponse>().await?;
            if status.try_again_later() {
                _next_instant = Instant::now() + Duration::from_secs(1);
                continue; // Will try again in a second
            }
            _next_instant = Instant::now() + Duration::from_millis(200);
            anyhow::ensure!(response.message == "token successfully destroyed", lazy_format!("{status}: Logout error: {}", response.message));
        }
    }

    // Section 4: Joining episode tasks
    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok(result) => if let Err(e) = result {
                println!("Error from an async task: {:?}", e);
            }
            Err(join_err) => {
                println!("Error when joining an async task: {:?}", join_err);
            }
        }
    }

    println!("");
    Ok(())
}
