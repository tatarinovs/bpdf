//! Interactive stamp placement: a local page in a Chromium app window shows
//! the rendered pages; the stamp is dragged and resized there and the final
//! placements are posted back.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use lopdf::Document;
use serde::Deserialize;
use serde_json::json;

use crate::pdf::transform::{self, PageGeometry, StampOptions, StampPlacement, StampSize};
use crate::{html, output, winpdf};

const PAGE: &str = include_str!("stamp_picker.html");
const PREVIEW_LONG_EDGE: u32 = 1600;
/// The browser has this long to load the page.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
/// The page pings every few seconds; silence this long means it was closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// A closed window cancels unless the page comes back (a reload) this soon.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// A stamp on the displayed page: fractions of its width and height, origin
/// at the top-left corner.
#[derive(Debug, Deserialize)]
struct PickedStamp {
    page: u32,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct PickerPage {
    crop: PageGeometry,
    /// Displayed size in points.
    width: f64,
    height: f64,
    paper: f64,
}

/// Lets the user place the stamp; `None` when the picker was cancelled.
pub fn pick(
    input: &Path,
    options: &StampOptions,
    browser: Option<&Path>,
) -> Result<Option<Vec<StampPlacement>>> {
    let document = crate::pdf::load(input)?;
    let bytes = std::fs::read(&options.path)
        .with_context(|| format!("failed to read stamp {}", options.path.display()))?;
    let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()?
        .into_dimensions()
        .with_context(|| format!("failed to decode stamp {}", options.path.display()))?;
    let size = StampSize::new(&bytes, dimensions, options.dpi);

    let (pages, initial) = describe_pages(&document, options, size)?;
    let info = json!({
        "file": input.file_name().map(|name| name.to_string_lossy()),
        "pages": pages.iter().map(|page| json!({
            "width": page.width,
            "height": page.height,
            "paper": page.paper,
        })).collect::<Vec<_>>(),
        "stamps": initial,
        "aspect": size.width / size.height,
        "opacity": options.opacity,
        "blend": css_blend(&format!("{:?}", options.blend_mode)),
    });

    let picked = serve(input, &bytes, &info.to_string(), pages.len(), browser)?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    picked
        .iter()
        .map(|stamp| {
            let page = pages
                .get((stamp.page as usize).wrapping_sub(1))
                .with_context(|| format!("stamp page {} does not exist", stamp.page))?;
            Ok(StampPlacement {
                page: stamp.page,
                matrix: placement_matrix(page, stamp),
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

/// Page sizes plus the starting stamps: one per page given by `--pages`,
/// or only on the last page for the default `all`.
fn describe_pages(
    document: &Document,
    options: &StampOptions,
    size: StampSize,
) -> Result<(Vec<PickerPage>, Vec<serde_json::Value>)> {
    let page_ids = document.get_pages();
    let selected = if options.pages.trim().eq_ignore_ascii_case("all") {
        vec![page_ids.len()]
    } else {
        crate::pdf::parse_page_selection(&options.pages, page_ids.len())?
            .into_iter()
            .collect()
    };
    let mut pages = Vec::with_capacity(page_ids.len());
    let mut initial = Vec::new();
    for (&number, &page_id) in &page_ids {
        let media = transform::page_geometry(document, page_id)?;
        let crop = transform::page_crop_geometry(document, page_id)?;
        let paper = transform::scan_paper_factor(document, page_id, media);
        let display = crop.displayed();
        let (width, height) = (display.raw_width(), display.raw_height());
        if selected.contains(&(number as usize)) {
            let (stamp_width, stamp_height) = size.fitted(options.scale, display, paper);
            let (left, bottom) = transform::stamp_position(
                &options.position,
                display,
                stamp_width,
                stamp_height,
                paper,
            )?;
            initial.push(json!({
                "page": number,
                "x": left / width,
                "y": 1.0 - (bottom + stamp_height) / height,
                "w": stamp_width / width,
                "h": stamp_height / height,
            }));
        }
        pages.push(PickerPage {
            crop,
            width,
            height,
            paper,
        });
    }
    Ok((pages, initial))
}

/// The `cm` matrix drawing the unit image square upright on the displayed
/// page, whatever the page rotation.
fn placement_matrix(page: &PickerPage, stamp: &PickedStamp) -> [f64; 6] {
    let left = stamp.x * page.width;
    let width = stamp.w * page.width;
    let height = stamp.h * page.height;
    let bottom = (1.0 - stamp.y) * page.height - height;
    let (origin_x, origin_y) = page.crop.display_to_page(left, bottom);
    let (right_x, right_y) = page.crop.display_to_page(left + width, bottom);
    let (top_x, top_y) = page.crop.display_to_page(left, bottom + height);
    [
        right_x - origin_x,
        right_y - origin_y,
        top_x - origin_x,
        top_y - origin_y,
        origin_x,
        origin_y,
    ]
}

fn css_blend(mode: &str) -> String {
    match mode.to_ascii_lowercase().as_str() {
        "colordodge" => "color-dodge".to_owned(),
        "colorburn" => "color-burn".to_owned(),
        "hardlight" => "hard-light".to_owned(),
        "softlight" => "soft-light".to_owned(),
        other => other.to_owned(),
    }
}

/// Runs the local server until the page posts placements or is closed.
fn serve(
    input: &Path,
    stamp: &[u8],
    info: &str,
    page_count: usize,
    browser: Option<&Path>,
) -> Result<Option<Vec<PickedStamp>>> {
    let listener = TcpListener::bind("127.0.0.1:0").context("failed to start the stamp picker")?;
    listener.set_nonblocking(true)?;
    let token = format!("{:016x}", {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(Instant::now().elapsed().as_nanos() ^ u128::from(std::process::id()));
        hasher.finish()
    });
    let url = format!("http://{}/{token}/", listener.local_addr()?);
    output::info(format!("Stamp picker: {url}"));
    open_browser(&url, browser);

    let mut previews: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut last_seen = Instant::now();
    let mut connected = false;
    let mut closed_at: Option<Instant> = None;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if closed_at.is_some_and(|closed| closed.elapsed() > CLOSE_GRACE) {
                    return Ok(None);
                }
                let timeout = if connected {
                    IDLE_TIMEOUT
                } else {
                    STARTUP_TIMEOUT
                };
                if last_seen.elapsed() > timeout {
                    bail!("the stamp picker window did not respond; open {url} to retry");
                }
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        last_seen = Instant::now();
        let Ok(request) = read_request(&stream) else {
            continue;
        };
        let Some(route) = request.path.strip_prefix(&format!("/{token}/")) else {
            respond(&stream, "404 Not Found", "text/plain", b"not found");
            continue;
        };
        connected = true;
        closed_at = None;
        match (request.method.as_str(), route) {
            ("GET", "") => respond(
                &stream,
                "200 OK",
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            ),
            ("GET", "info") => respond(&stream, "200 OK", "application/json", info.as_bytes()),
            ("GET", "stamp") => respond(&stream, "200 OK", image_type(stamp), stamp),
            ("GET", page) if page.starts_with("page/") => {
                let number = page["page/".len()..].parse::<usize>().unwrap_or(0);
                if number == 0 || number > page_count {
                    respond(&stream, "404 Not Found", "text/plain", b"no such page");
                    continue;
                }
                let preview = match previews.entry(number) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        match winpdf::render_pdf_preview(input, number, PREVIEW_LONG_EDGE) {
                            Ok(jpeg) => entry.insert(jpeg),
                            Err(error) => {
                                let message = format!("{error:#}");
                                respond(
                                    &stream,
                                    "500 Internal Server Error",
                                    "text/plain",
                                    message.as_bytes(),
                                );
                                continue;
                            }
                        }
                    }
                };
                respond(&stream, "200 OK", "image/jpeg", preview);
            }
            ("POST", "ping") => respond(&stream, "204 No Content", "text/plain", b""),
            ("POST", "closed") => {
                respond(&stream, "204 No Content", "text/plain", b"");
                closed_at = Some(Instant::now());
            }
            ("POST", "cancel") => {
                respond(&stream, "204 No Content", "text/plain", b"");
                return Ok(None);
            }
            ("POST", "apply") => {
                let stamps: Vec<PickedStamp> = match serde_json::from_slice(&request.body) {
                    Ok(stamps) => stamps,
                    Err(error) => {
                        let message = format!("invalid placements: {error}");
                        respond(&stream, "400 Bad Request", "text/plain", message.as_bytes());
                        continue;
                    }
                };
                respond(&stream, "204 No Content", "text/plain", b"");
                return Ok((!stamps.is_empty()).then_some(stamps));
            }
            _ => respond(&stream, "404 Not Found", "text/plain", b"not found"),
        }
    }
}

fn open_browser(url: &str, configured: Option<&Path>) {
    let launched = html::find_browser(configured).and_then(|browser| {
        std::process::Command::new(browser)
            .arg(format!("--app={url}"))
            .arg("--window-size=1280,1000")
            .spawn()
            .map(drop)
            .map_err(Into::into)
    });
    if launched.is_ok() {
        return;
    }
    #[cfg(windows)]
    let fallback = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
    #[cfg(not(windows))]
    let fallback = std::process::Command::new("xdg-open").arg(url).spawn();
    if fallback.is_err() {
        output::warn(format!("open {url} in a browser to place the stamp"));
    }
}

fn image_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        "image/jpeg"
    } else {
        "application/octet-stream"
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(mut stream: &TcpStream) -> Result<Request> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("connection closed");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 64 * 1024 {
            bail!("request headers are too large");
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut start = lines.next().unwrap_or_default().split_whitespace();
    let method = start.next().unwrap_or_default().to_owned();
    let path = start.next().unwrap_or_default().to_owned();
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if length > 1024 * 1024 {
        bail!("request body is too large");
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Request { method, path, body })
}

fn respond(mut stream: &TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: private, max-age=3600\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(rotation: i64) -> PickerPage {
        let crop = PageGeometry {
            left: 0.0,
            bottom: 0.0,
            right: 600.0,
            top: 800.0,
            rotation,
        };
        let display = crop.displayed();
        PickerPage {
            crop,
            width: display.raw_width(),
            height: display.raw_height(),
            paper: 1.0,
        }
    }

    #[test]
    fn upright_page_maps_fractions_to_points() {
        let stamp = PickedStamp {
            page: 1,
            x: 0.5,
            y: 0.75,
            w: 0.25,
            h: 0.125,
        };
        // 150x100 pt, top edge at 25% from the bottom.
        assert_eq!(
            placement_matrix(&page(0), &stamp),
            [150.0, 0.0, 0.0, 100.0, 300.0, 100.0]
        );
    }

    #[test]
    fn rotated_page_keeps_the_stamp_upright_on_screen() {
        let stamp = PickedStamp {
            page: 1,
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let [a, b, c, d, ..] = placement_matrix(&page(90), &stamp);
        // The displayed page is 800x600; image X runs along the raw Y axis.
        assert_eq!((a, b, c, d), (0.0, 400.0, -300.0, 0.0));
    }
}
