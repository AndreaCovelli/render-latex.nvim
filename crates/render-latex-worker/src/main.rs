use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow};
use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_parser::parser::parse;
use ratex_render::{RenderOptions, render_to_png};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Response {
    id: u64,
    result: Option<Value>,
    error: Option<ResponseError>,
}

#[derive(Debug, Serialize)]
struct ResponseError {
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseValue {
    cache_key: String,
    png_path: String,
    width_px: u32,
    height_px: u32,
    diagnostics: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RenderParams {
    formula: String,
    #[serde(default = "default_true")]
    display_mode: bool,
    foreground_color: Option<String>,
    background: Option<String>,
    font_size: Option<f32>,
    padding: Option<f32>,
    scale: Option<f32>,
    theme_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InvalidateParams {
    cache_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RenderBatchParams {
    items: Vec<RenderParams>,
}

#[derive(Clone)]
struct CacheEntry {
    value: ResponseValue,
    last_used: u64,
}

struct Worker {
    cache_dir: PathBuf,
    memory_cache: HashMap<String, CacheEntry>,
    access_tick: u64,
    disk_prune_tick: u64,
}

const MAX_MEMORY_CACHE_ENTRIES: usize = 256;
const MAX_DISK_CACHE_ENTRIES: usize = 2048;
const DISK_PRUNE_INTERVAL: u64 = 32;

fn default_true() -> bool {
    true
}

impl Worker {
    fn new() -> Result<Self> {
        let cache_dir = resolve_cache_dir()?;
        fs::create_dir_all(&cache_dir)?;

        let mut worker = Self {
            cache_dir,
            memory_cache: HashMap::new(),
            access_tick: 0,
            disk_prune_tick: 0,
        };
        worker.warm_fonts()?;
        Ok(worker)
    }

    fn next_tick(&mut self) -> u64 {
        self.access_tick = self.access_tick.saturating_add(1);
        self.access_tick
    }

    fn cache_get(&mut self, cache_key: &str) -> Option<ResponseValue> {
        let tick = self.next_tick();
        self.memory_cache.get_mut(cache_key).and_then(|entry| {
            if Path::new(&entry.value.png_path).exists() {
                entry.last_used = tick;
                Some(entry.value.clone())
            } else {
                None
            }
        })
    }

    fn cache_insert(&mut self, cache_key: String, value: ResponseValue) {
        let tick = self.next_tick();
        self.memory_cache.insert(
            cache_key,
            CacheEntry {
                value,
                last_used: tick,
            },
        );
        self.prune_memory_cache();
    }

    fn prune_memory_cache(&mut self) {
        while self.memory_cache.len() > MAX_MEMORY_CACHE_ENTRIES {
            let oldest_key = self
                .memory_cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());

            if let Some(key) = oldest_key {
                self.memory_cache.remove(&key);
            } else {
                break;
            }
        }
    }

    fn maybe_prune_disk_cache(&mut self) -> Result<()> {
        self.disk_prune_tick = self.disk_prune_tick.saturating_add(1);
        if !self.disk_prune_tick.is_multiple_of(DISK_PRUNE_INTERVAL) {
            return Ok(());
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let modified = entry
                .metadata()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((path, modified));
        }

        if entries.len() <= MAX_DISK_CACHE_ENTRIES {
            return Ok(());
        }

        entries.sort_by_key(|(_, modified)| *modified);
        let remove_count = entries.len() - MAX_DISK_CACHE_ENTRIES;
        for (metadata_path, _) in entries.into_iter().take(remove_count) {
            if let Some(stem) = metadata_path.file_stem().and_then(|stem| stem.to_str()) {
                self.memory_cache.remove(stem);
                let png_path = self.cache_dir.join(format!("{}.png", stem));
                let _ = fs::remove_file(&metadata_path);
                let _ = fs::remove_file(png_path);
            }
        }

        Ok(())
    }

    fn warm_fonts(&mut self) -> Result<()> {
        let params = RenderParams {
            formula: "x".to_string(),
            display_mode: true,
            foreground_color: Some("#ffffff".to_string()),
            background: Some("transparent".to_string()),
            font_size: Some(34.0),
            padding: Some(4.0),
            scale: Some(1.0),
            theme_fingerprint: Some("warmup".to_string()),
        };
        let _ = self.render(params)?;
        Ok(())
    }

    fn render(&mut self, params: RenderParams) -> Result<ResponseValue> {
        let cache_key = build_cache_key(&params)?;
        if let Some(cached) = self.cache_get(&cache_key) {
            return Ok(cached);
        }

        if let Some(cached) = self.load_disk_cache(&cache_key)? {
            self.cache_insert(cache_key.clone(), cached.clone());
            return Ok(cached);
        }

        let foreground = parse_color(params.foreground_color.as_deref().unwrap_or("#ffffff"))?;
        let background = parse_color(params.background.as_deref().unwrap_or("transparent"))?;
        let ast = parse(&params.formula).map_err(|err| anyhow!(err.to_string()))?;
        let options = LayoutOptions {
            style: if params.display_mode {
                MathStyle::Display
            } else {
                MathStyle::Text
            },
            color: foreground,
            ..LayoutOptions::default()
        };
        let layout_box = layout(&ast, &options);
        let display_list = to_display_list(&layout_box);
        let png = render_to_png(
            &display_list,
            &RenderOptions {
                font_size: params.font_size.unwrap_or(34.0),
                padding: params.padding.unwrap_or(10.0),
                background_color: background,
                font_dir: String::new(),
                device_pixel_ratio: params.scale.unwrap_or(1.5),
            },
        )
        .map_err(|err| anyhow!(err))?;

        let png_path = self.cache_dir.join(format!("{}.png", cache_key));
        let metadata_path = self.cache_dir.join(format!("{}.json", cache_key));
        fs::write(&png_path, &png)?;
        let (width_px, height_px) = png_dimensions(&png)?;

        let response = ResponseValue {
            cache_key: cache_key.clone(),
            png_path: png_path.to_string_lossy().into_owned(),
            width_px,
            height_px,
            diagnostics: None,
        };

        fs::write(&metadata_path, serde_json::to_vec(&response)?)?;
        self.cache_insert(cache_key, response.clone());
        self.maybe_prune_disk_cache()?;

        Ok(response)
    }

    fn invalidate(&mut self, params: InvalidateParams) -> Result<ResponseValue> {
        if let Some(cache_key) = params.cache_key {
            self.memory_cache.remove(&cache_key);
            let png_path = self.cache_dir.join(format!("{}.png", cache_key));
            let metadata_path = self.cache_dir.join(format!("{}.json", cache_key));
            let _ = fs::remove_file(png_path);
            let _ = fs::remove_file(metadata_path);
            return Ok(ResponseValue {
                cache_key,
                png_path: String::new(),
                width_px: 0,
                height_px: 0,
                diagnostics: Some("invalidated cache entry".to_string()),
            });
        }

        self.memory_cache.clear();
        Ok(ResponseValue {
            cache_key: "*".to_string(),
            png_path: String::new(),
            width_px: 0,
            height_px: 0,
            diagnostics: Some("cleared memory cache".to_string()),
        })
    }

    fn load_disk_cache(&self, cache_key: &str) -> Result<Option<ResponseValue>> {
        let metadata_path = self.cache_dir.join(format!("{}.json", cache_key));
        if !metadata_path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(metadata_path)?;
        let value = serde_json::from_slice::<ResponseValue>(&bytes)?;
        if !Path::new(&value.png_path).exists() {
            return Ok(None);
        }
        Ok(Some(value))
    }
}

fn resolve_cache_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("RENDER_LATEX_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let mut base = if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache")
    } else {
        return Err(anyhow!("unable to resolve cache directory"));
    };

    base.push("render-latex");
    base.push("worker");
    Ok(base)
}

fn build_cache_key(params: &RenderParams) -> Result<String> {
    let json = serde_json::to_vec(params)?;
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(json);
    Ok(hex::encode(hasher.finalize()))
}

fn parse_color(value: &str) -> Result<Color> {
    Color::parse(value).ok_or_else(|| anyhow!("invalid color: {}", value))
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info()?;
    let info = reader.info();
    Ok((info.width, info.height))
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Request>> {
    let mut header = String::new();
    if reader.read_line(&mut header)? == 0 {
        return Ok(None);
    }
    if header.trim().is_empty() {
        return Ok(None);
    }

    let length = header
        .strip_prefix("Content-Length: ")
        .ok_or_else(|| anyhow!("missing content-length header"))?
        .trim()
        .parse::<usize>()?;

    let mut blank = String::new();
    reader.read_line(&mut blank)?;

    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

fn write_message<W: Write>(writer: &mut W, response: &Response) -> Result<()> {
    let body = serde_json::to_vec(response)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn handle_request(worker: &mut Worker, request: Request) -> Response {
    match request.method.as_str() {
        "ping" => Response {
            id: request.id,
            result: Some(json!(ResponseValue {
                cache_key: "ping".to_string(),
                png_path: String::new(),
                width_px: 0,
                height_px: 0,
                diagnostics: Some("pong".to_string()),
            })),
            error: None,
        },
        "render" => match serde_json::from_value::<RenderParams>(request.params)
            .context("invalid render params")
            .and_then(|params| worker.render(params))
        {
            Ok(result) => Response {
                id: request.id,
                result: Some(json!(result)),
                error: None,
            },
            Err(err) => Response {
                id: request.id,
                result: None,
                error: Some(ResponseError {
                    message: err.to_string(),
                }),
            },
        },
        "render_batch" => match serde_json::from_value::<RenderBatchParams>(request.params)
            .context("invalid render_batch params")
        {
            Ok(params) => {
                let mut results = Vec::with_capacity(params.items.len());
                for item in params.items {
                    match worker.render(item) {
                        Ok(result) => results.push(json!({ "result": result, "error": Value::Null })),
                        Err(err) => results.push(json!({ "result": Value::Null, "error": { "message": err.to_string() } })),
                    }
                }

                Response {
                    id: request.id,
                    result: Some(Value::Array(results)),
                    error: None,
                }
            }
            Err(err) => Response {
                id: request.id,
                result: None,
                error: Some(ResponseError {
                    message: err.to_string(),
                }),
            },
        },
        "invalidate" => match serde_json::from_value::<InvalidateParams>(request.params)
            .context("invalid invalidate params")
            .and_then(|params| worker.invalidate(params))
        {
            Ok(result) => Response {
                id: request.id,
                result: Some(json!(result)),
                error: None,
            },
            Err(err) => Response {
                id: request.id,
                result: None,
                error: Some(ResponseError {
                    message: err.to_string(),
                }),
            },
        },
        "shutdown" => Response {
            id: request.id,
            result: Some(json!(ResponseValue {
                cache_key: "shutdown".to_string(),
                png_path: String::new(),
                width_px: 0,
                height_px: 0,
                diagnostics: Some("shutting down".to_string()),
            })),
            error: None,
        },
        _ => Response {
            id: request.id,
            result: None,
            error: Some(ResponseError {
                message: "unknown method".to_string(),
            }),
        },
    }
}

fn main() -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut writer = io::BufWriter::new(stdout.lock());
    let mut worker = Worker::new()?;

    while let Some(request) = read_message(&mut reader)? {
        let shutdown = request.method == "shutdown";
        let response = handle_request(&mut worker, request);
        write_message(&mut writer, &response)?;
        if shutdown {
            break;
        }
    }

    Ok(())
}
