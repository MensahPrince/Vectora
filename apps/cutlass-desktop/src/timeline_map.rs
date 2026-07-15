//! Deterministic schematic timeline rendering for vision-model context.
//!
//! This deliberately renders model semantics instead of mirroring Slint pixels:
//! it stays useful when the editor is off-screen and has no UI, font, filesystem,
//! decoder, or GPU dependencies.

use cutlass_models::{Clip, ClipSource, Generator, Project, Track, TrackKind};
use cutlass_render::RgbaImage;

const MIN_WIDTH: u32 = 320;
const MAX_WIDTH: u32 = 1024;
const MAX_HEIGHT: u32 = 768;
const LABEL_GUTTER: i32 = 142;
const RIGHT_MARGIN: i32 = 8;
const RULER_HEIGHT: i32 = 38;
const LEGEND_HEIGHT: i32 = 24;
const LANE_HEIGHT: i32 = 48;
const MAX_DISPLAY_ROWS: usize =
    ((MAX_HEIGHT as i32 - RULER_HEIGHT - LEGEND_HEIGHT) / LANE_HEIGHT) as usize;

const BACKGROUND: [u8; 4] = [18, 20, 25, 255];
const RULER_BACKGROUND: [u8; 4] = [27, 30, 37, 255];
const GUTTER_BACKGROUND: [u8; 4] = [24, 27, 33, 255];
const LANE_BACKGROUND_A: [u8; 4] = [29, 32, 39, 255];
const LANE_BACKGROUND_B: [u8; 4] = [32, 35, 43, 255];
const OMISSION_BACKGROUND: [u8; 4] = [55, 43, 65, 255];
const GRID: [u8; 4] = [49, 53, 64, 255];
const DIVIDER: [u8; 4] = [66, 71, 84, 255];
const PRIMARY_TEXT: [u8; 4] = [238, 240, 245, 255];
const SECONDARY_TEXT: [u8; 4] = [169, 176, 190, 255];
const CLIP_TEXT: [u8; 4] = [250, 251, 253, 255];
const PLAYHEAD: [u8; 4] = [255, 59, 79, 255];

const TICK_INTERVALS: [f64; 12] = [
    0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
];

/// Controls the bounded timeline window rendered by [`render`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimelineMapOptions {
    pub width: u32,
    pub start_seconds: f64,
    pub end_seconds: Option<f64>,
    pub playhead_seconds: Option<f64>,
}

impl Default for TimelineMapOptions {
    fn default() -> Self {
        Self {
            width: 768,
            start_seconds: 0.0,
            end_seconds: None,
            playhead_seconds: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimeWindow {
    start: f64,
    end: f64,
}

impl TimeWindow {
    fn resolve(project: &Project, options: TimelineMapOptions) -> Result<Self, String> {
        if !options.start_seconds.is_finite() || options.start_seconds < 0.0 {
            return Err("timeline-map start must be finite and non-negative".into());
        }
        if options
            .playhead_seconds
            .is_some_and(|playhead| !playhead.is_finite())
        {
            return Err("timeline-map playhead must be finite".into());
        }

        let end = match options.end_seconds {
            Some(end) if !end.is_finite() => {
                return Err("timeline-map end must be finite".into());
            }
            Some(end) if end <= options.start_seconds => {
                return Err("timeline-map end must be after start".into());
            }
            Some(end) => end,
            None => {
                let minimum_end = options.start_seconds + 1.0;
                if !minimum_end.is_finite() || minimum_end <= options.start_seconds {
                    return Err("timeline-map start is too large for a one-second window".into());
                }
                let project_end = project.timeline().duration().seconds();
                if project_end.is_finite() {
                    project_end.max(minimum_end)
                } else {
                    minimum_end
                }
            }
        };

        Ok(Self {
            start: options.start_seconds,
            end,
        })
    }

    fn span(self) -> f64 {
        self.end - self.start
    }

    fn contains(self, seconds: f64) -> bool {
        seconds.is_finite() && seconds >= self.start && seconds <= self.end
    }
}

enum DisplayRow<'a> {
    Track(&'a Track),
    Omitted(usize),
    Empty,
}

/// Render a self-contained, packed straight-alpha RGBA8 timeline schematic.
pub(crate) fn render(project: &Project, options: TimelineMapOptions) -> Result<RgbaImage, String> {
    let window = TimeWindow::resolve(project, options)?;
    let width = options.width.clamp(MIN_WIDTH, MAX_WIDTH);

    let mut tracks: Vec<&Track> = project.timeline().tracks_ordered().collect();
    // Model order is the compositor's bottom-to-top stack; lane UIs expose the
    // front-most layer first so the schematic follows the editor's reading order.
    tracks.reverse();
    let rows = display_rows(&tracks);

    let row_count = u32::try_from(rows.len()).map_err(|_| "too many timeline-map rows")?;
    let height = (RULER_HEIGHT as u32)
        .checked_add(
            row_count
                .checked_mul(LANE_HEIGHT as u32)
                .ok_or("timeline-map height overflow")?,
        )
        .and_then(|height| height.checked_add(LEGEND_HEIGHT as u32))
        .ok_or("timeline-map height overflow")?;
    if height > MAX_HEIGHT {
        return Err("timeline-map height exceeds its bound".into());
    }

    let mut canvas = Canvas::new(width, height, BACKGROUND)?;
    let plot_left = LABEL_GUTTER;
    let plot_right = i32::try_from(width)
        .unwrap_or(i32::MAX)
        .saturating_sub(RIGHT_MARGIN)
        .max(plot_left + 1);
    let legend_top = i32::try_from(height)
        .unwrap_or(i32::MAX)
        .saturating_sub(LEGEND_HEIGHT);

    canvas.fill_rect(0, 0, width as i32, RULER_HEIGHT, RULER_BACKGROUND);
    canvas.fill_rect(0, 0, LABEL_GUTTER, RULER_HEIGHT, GUTTER_BACKGROUND);
    canvas.draw_text_clipped("TIMELINE MAP", 10, 8, LABEL_GUTTER - 18, 9, PRIMARY_TEXT, 1);
    canvas.draw_text_clipped(
        "TRACK / KIND / RAW ID",
        10,
        22,
        LABEL_GUTTER - 18,
        9,
        SECONDARY_TEXT,
        1,
    );

    draw_ruler(&mut canvas, window, plot_left, plot_right, legend_top);

    let mut intersecting_clips = 0usize;
    for (index, row) in rows.iter().enumerate() {
        let y = RULER_HEIGHT + i32::try_from(index).unwrap_or(i32::MAX) * LANE_HEIGHT;
        let lane_background = if index % 2 == 0 {
            LANE_BACKGROUND_A
        } else {
            LANE_BACKGROUND_B
        };
        canvas.fill_rect(
            plot_left,
            y,
            plot_right - plot_left,
            LANE_HEIGHT,
            lane_background,
        );
        canvas.fill_rect(0, y, LABEL_GUTTER, LANE_HEIGHT, GUTTER_BACKGROUND);
        canvas.fill_rect(0, y + LANE_HEIGHT - 1, plot_right, 1, DIVIDER);

        match row {
            DisplayRow::Track(track) => {
                let color = kind_color(track.kind);
                canvas.fill_rect(3, y + 5, 4, LANE_HEIGHT - 10, color);
                canvas.draw_text_clipped(
                    &track.name,
                    11,
                    y + 8,
                    LABEL_GUTTER - 18,
                    9,
                    PRIMARY_TEXT,
                    1,
                );
                let details = if track.main {
                    format!("{} #{} MAIN", kind_name(track.kind), track.id.raw())
                } else {
                    format!("{} #{}", kind_name(track.kind), track.id.raw())
                };
                canvas.draw_text_clipped(
                    &details,
                    11,
                    y + 25,
                    LABEL_GUTTER - 18,
                    9,
                    SECONDARY_TEXT,
                    1,
                );

                // Hash-map order is intentionally irrelevant: clips on one
                // lane do not overlap, so each writes disjoint pixels. Scanning
                // in place avoids sorting/allocating every off-window clip just
                // to draw the handful that intersect this map.
                let lane = LanePlot {
                    kind: track.kind,
                    window,
                    left: plot_left,
                    right: plot_right,
                    y,
                };
                for clip in track.clips() {
                    if draw_clip(&mut canvas, project, clip, lane) {
                        intersecting_clips += 1;
                    }
                }
            }
            DisplayRow::Omitted(count) => {
                canvas.fill_rect(0, y, plot_right, LANE_HEIGHT, OMISSION_BACKGROUND);
                let label = format!("... {count} LANES OMITTED ...");
                let text_width = text_width(&label, 1);
                let x = ((plot_right - text_width) / 2).max(5);
                canvas.draw_text_clipped(&label, x, y + 20, plot_right - x - 4, 9, PRIMARY_TEXT, 1);
            }
            DisplayRow::Empty => {
                canvas.draw_text_clipped(
                    "NO TRACKS",
                    11,
                    y + 20,
                    LABEL_GUTTER - 18,
                    9,
                    SECONDARY_TEXT,
                    1,
                );
                canvas.draw_text_clipped(
                    "EMPTY TIMELINE",
                    plot_left + 10,
                    y + 20,
                    plot_right - plot_left - 18,
                    9,
                    SECONDARY_TEXT,
                    1,
                );
            }
        }
    }

    draw_markers(
        &mut canvas,
        project,
        window,
        plot_left,
        plot_right,
        legend_top,
    );
    if let Some(playhead) = options
        .playhead_seconds
        .filter(|playhead| window.contains(*playhead))
    {
        draw_playhead(
            &mut canvas,
            playhead,
            window,
            plot_left,
            plot_right,
            legend_top,
        );
    }

    canvas.fill_rect(
        0,
        legend_top,
        i32::try_from(width).unwrap_or(i32::MAX),
        LEGEND_HEIGHT,
        RULER_BACKGROUND,
    );
    canvas.fill_rect(0, legend_top, width as i32, 1, DIVIDER);
    let status = format!(
        "SCHEMATIC | {}-{} | {} LANES | {} CLIPS | {} MARKERS",
        format_compact_time(window.start),
        format_compact_time(window.end),
        tracks.len(),
        intersecting_clips,
        project.timeline().markers().len()
    );
    canvas.draw_text_clipped(
        &status,
        10,
        legend_top + 8,
        width as i32 - 20,
        9,
        SECONDARY_TEXT,
        1,
    );

    Ok(canvas.into_image())
}

fn display_rows<'a>(tracks: &[&'a Track]) -> Vec<DisplayRow<'a>> {
    if tracks.is_empty() {
        return vec![DisplayRow::Empty];
    }
    if tracks.len() <= MAX_DISPLAY_ROWS {
        return tracks.iter().copied().map(DisplayRow::Track).collect();
    }

    let visible_slots = MAX_DISPLAY_ROWS.saturating_sub(1);
    let mut selected: Vec<usize> = (0..visible_slots).collect();
    if let Some(main_index) = tracks.iter().position(|track| track.main)
        && !selected.contains(&main_index)
        && let Some(last) = selected.last_mut()
    {
        *last = main_index;
    }
    selected.sort_unstable();
    selected.dedup();

    let omitted = tracks.len().saturating_sub(selected.len());
    let omission_at = selected
        .windows(2)
        .position(|pair| pair[1] > pair[0] + 1)
        .map_or(selected.len(), |index| index + 1);
    let mut rows = Vec::with_capacity(selected.len() + 1);
    for (position, track_index) in selected.into_iter().enumerate() {
        if position == omission_at {
            rows.push(DisplayRow::Omitted(omitted));
        }
        rows.push(DisplayRow::Track(tracks[track_index]));
    }
    if omission_at == rows.len() {
        rows.push(DisplayRow::Omitted(omitted));
    }
    rows
}

fn draw_ruler(
    canvas: &mut Canvas,
    window: TimeWindow,
    plot_left: i32,
    plot_right: i32,
    lane_bottom: i32,
) {
    canvas.fill_rect(
        plot_left,
        RULER_HEIGHT - 1,
        plot_right - plot_left,
        1,
        DIVIDER,
    );
    canvas.fill_rect(LABEL_GUTTER - 1, 0, 1, lane_bottom, DIVIDER);

    let interval = choose_tick_interval(window.span());
    let first_index = (window.start / interval).ceil();
    let last_index = (window.end / interval).floor();
    if !first_index.is_finite() || !last_index.is_finite() || last_index < first_index {
        return;
    }

    let tick_count = (last_index - first_index + 1.0).max(1.0);
    let budget = usize::try_from((plot_right - plot_left).max(1)).unwrap_or(1);
    let stride = (tick_count / budget as f64).ceil().max(1.0);
    let mut last_x = i32::MIN;
    let mut last_label_right = i32::MIN;

    // The pixel-width budget prevents extreme but finite ranges from turning
    // the fixed interval list into a duration-proportional loop.
    for step in 0..=budget {
        let index = first_index + step as f64 * stride;
        if index > last_index + 0.5 {
            break;
        }
        let tick = index * interval;
        if !tick.is_finite() {
            continue;
        }
        let x = time_to_x(tick, window, plot_left, plot_right);
        if x == last_x {
            continue;
        }
        last_x = x;
        canvas.fill_rect(x, RULER_HEIGHT - 7, 1, 6, SECONDARY_TEXT);
        canvas.fill_rect(x, RULER_HEIGHT, 1, lane_bottom - RULER_HEIGHT, GRID);

        let label = format_ruler_time(tick, interval);
        let width = text_width(&label, 1);
        let label_x = (x - width / 2).clamp(plot_left, (plot_right - width).max(plot_left));
        if label_x > last_label_right + 2 {
            canvas.draw_text_clipped(&label, label_x, 6, plot_right - label_x, 9, PRIMARY_TEXT, 1);
            last_label_right = label_x + width;
        }
    }
}

#[derive(Clone, Copy)]
struct LanePlot {
    kind: TrackKind,
    window: TimeWindow,
    left: i32,
    right: i32,
    y: i32,
}

fn draw_clip(canvas: &mut Canvas, project: &Project, clip: &Clip, lane: LanePlot) -> bool {
    let start = clip.timeline.start.seconds();
    let duration = clip.timeline.duration.seconds();
    let end = start + duration;
    if !start.is_finite()
        || !duration.is_finite()
        || !end.is_finite()
        || duration <= 0.0
        || end <= lane.window.start
        || start >= lane.window.end
    {
        return false;
    }

    let visible_start = start.max(lane.window.start);
    let visible_end = end.min(lane.window.end);
    let x0 = time_to_x_f64(visible_start, lane.window, lane.left, lane.right)
        .floor()
        .clamp(f64::from(lane.left), f64::from(lane.right - 1)) as i32;
    let mut x1 = time_to_x_f64(visible_end, lane.window, lane.left, lane.right)
        .ceil()
        .clamp(f64::from(lane.left + 1), f64::from(lane.right)) as i32;
    x1 = x1.max(x0 + 1).min(lane.right);

    let clip_y = lane.y + 6;
    let clip_height = LANE_HEIGHT - 12;
    let clip_width = x1 - x0;
    canvas.fill_rect(x0, clip_y, clip_width, clip_height, kind_color(lane.kind));

    if clip_width >= 9 {
        let label = clip_label(project, clip);
        canvas.draw_text_clipped(&label, x0 + 4, clip_y + 5, clip_width - 8, 9, CLIP_TEXT, 1);
    }
    if clip_width >= 74 {
        let timing = format!(
            "{}-{}",
            format_compact_time(start),
            format_compact_time(end)
        );
        canvas.draw_text_clipped(
            &timing,
            x0 + 4,
            clip_y + 20,
            clip_width - 8,
            9,
            CLIP_TEXT,
            1,
        );
    }
    true
}

fn draw_markers(
    canvas: &mut Canvas,
    project: &Project,
    window: TimeWindow,
    plot_left: i32,
    plot_right: i32,
    lane_bottom: i32,
) {
    let mut visible_index = 0usize;
    for marker in project.timeline().markers() {
        let seconds = marker.tick.seconds();
        if !window.contains(seconds) {
            continue;
        }
        let x = time_to_x(seconds, window, plot_left, plot_right);
        let color = marker.color.rgba();
        canvas.fill_rect(
            x,
            RULER_HEIGHT - 2,
            1,
            lane_bottom - RULER_HEIGHT + 2,
            color,
        );
        canvas.fill_rect(x - 2, RULER_HEIGHT - 6, 5, 4, color);

        let mut label = format!("M#{} ", marker.id.raw());
        append_escaped_bounded(&mut label, &marker.name, 28);
        let label_y = 18 + i32::try_from(visible_index % 2).unwrap_or(0) * 9;
        canvas.draw_text_clipped(&label, x + 3, label_y, plot_right - x - 4, 8, color, 1);
        visible_index += 1;
    }
}

fn draw_playhead(
    canvas: &mut Canvas,
    seconds: f64,
    window: TimeWindow,
    plot_left: i32,
    plot_right: i32,
    lane_bottom: i32,
) {
    let x = time_to_x(seconds, window, plot_left, plot_right);
    canvas.fill_rect(
        x,
        RULER_HEIGHT - 1,
        2,
        lane_bottom - RULER_HEIGHT + 1,
        PLAYHEAD,
    );
    for row in 0..6 {
        canvas.fill_rect(x - row / 2, RULER_HEIGHT - 8 + row, 3 + row, 1, PLAYHEAD);
    }
}

fn choose_tick_interval(span: f64) -> f64 {
    let mut best = TICK_INTERVALS[0];
    let mut best_score = f64::INFINITY;
    for interval in TICK_INTERVALS {
        let labels = span / interval;
        let outside_penalty = if labels < 7.0 {
            (7.0 - labels) * 10.0
        } else if labels > 12.0 {
            (labels - 12.0) * 10.0
        } else {
            0.0
        };
        let score = outside_penalty + (labels - 9.0).abs();
        if score < best_score {
            best = interval;
            best_score = score;
        }
    }
    best
}

fn time_to_x(seconds: f64, window: TimeWindow, plot_left: i32, plot_right: i32) -> i32 {
    time_to_x_f64(seconds, window, plot_left, plot_right)
        .round()
        .clamp(f64::from(plot_left), f64::from(plot_right - 1)) as i32
}

fn time_to_x_f64(seconds: f64, window: TimeWindow, plot_left: i32, plot_right: i32) -> f64 {
    let fraction = ((seconds - window.start) / window.span()).clamp(0.0, 1.0);
    f64::from(plot_left) + fraction * f64::from(plot_right - plot_left)
}

fn kind_color(kind: TrackKind) -> [u8; 4] {
    match kind {
        TrackKind::Video => [0x4A, 0x6F, 0xA5, 0xFF],
        TrackKind::Audio => [0xC9, 0x98, 0x46, 0xFF],
        TrackKind::Text => [0x5E, 0x8B, 0x7E, 0xFF],
        TrackKind::Sticker => [0xBF, 0x6F, 0x4A, 0xFF],
        TrackKind::Effect => [0x7B, 0x68, 0xA6, 0xFF],
        TrackKind::Filter => [0x4A, 0x8C, 0x8C, 0xFF],
        TrackKind::Adjustment => [0x6C, 0x5B, 0x7B, 0xFF],
    }
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "VIDEO",
        TrackKind::Audio => "AUDIO",
        TrackKind::Text => "TEXT",
        TrackKind::Sticker => "STICKER",
        TrackKind::Effect => "EFFECT",
        TrackKind::Filter => "FILTER",
        TrackKind::Adjustment => "ADJUSTMENT",
    }
}

fn clip_label(project: &Project, clip: &Clip) -> String {
    let mut label = format!("#{} ", clip.id.raw());
    match &clip.content {
        ClipSource::Media { media, .. } => {
            let name = project
                .media(*media)
                .and_then(|source| source.path().file_name())
                .and_then(|name| name.to_str());
            if let Some(name) = name {
                append_escaped_bounded(&mut label, name, 48);
            } else {
                label.push_str("MISSING MEDIA");
            }
        }
        ClipSource::Generated(generator) => match generator {
            Generator::Text { content, .. } => {
                label.push_str("TEXT \"");
                append_escaped_bounded(&mut label, content, 32);
                label.push('"');
            }
            Generator::SolidColor { .. } => label.push_str("SOLID"),
            Generator::Shape { .. } => label.push_str("SHAPE"),
            Generator::Sticker { .. } => label.push_str("STICKER"),
            Generator::Lottie { .. } => label.push_str("LOTTIE"),
            Generator::Effect => label.push_str("EFFECT"),
            Generator::Filter => label.push_str("FILTER"),
            Generator::Adjustment => label.push_str("ADJUSTMENT"),
        },
    }
    label
}

fn append_escaped_bounded(output: &mut String, input: &str, max_chars: usize) {
    let mut written = 0usize;
    for character in input.chars() {
        let escaped: &[char] = match character {
            '\n' => &['\\', 'n'],
            '\r' => &['\\', 'r'],
            '\t' => &['\\', 't'],
            character if character.is_control() => &['?'],
            _ => {
                if written == max_chars {
                    break;
                }
                output.push(character);
                written += 1;
                continue;
            }
        };
        if written + escaped.len() > max_chars {
            break;
        }
        for character in escaped {
            output.push(*character);
        }
        written += escaped.len();
    }
}

fn format_compact_time(seconds: f64) -> String {
    if seconds >= 60.0 {
        let minutes = (seconds / 60.0).floor();
        let remainder = seconds - minutes * 60.0;
        if (remainder - remainder.round()).abs() < 0.005 {
            format!("{minutes:.0}:{remainder:02.0}")
        } else {
            format!("{minutes:.0}:{remainder:04.1}")
        }
    } else if (seconds - seconds.round()).abs() < 0.005 {
        format!("{seconds:.0}s")
    } else {
        format!("{seconds:.1}s")
    }
}

fn format_ruler_time(seconds: f64, interval: f64) -> String {
    if seconds >= 60.0 {
        return format_compact_time(seconds);
    }
    if (seconds - seconds.round()).abs() < 0.005 {
        format!("{seconds:.0}s")
    } else if (interval - 0.25).abs() < f64::EPSILON {
        format!("{seconds:.2}s")
    } else {
        format!("{seconds:.1}s")
    }
}

pub(crate) struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    pub(crate) fn new(width: u32, height: u32, background: [u8; 4]) -> Result<Self, String> {
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err("invalid schematic-canvas dimensions".into());
        }
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("schematic-canvas allocation size overflow")?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| "unable to allocate schematic-canvas pixels")?;
        bytes.resize(len, 0);
        for pixel in bytes.chunks_exact_mut(4) {
            pixel.copy_from_slice(&background);
        }
        Ok(Self {
            width,
            height,
            pixels: bytes,
        })
    }

    pub(crate) fn into_image(self) -> RgbaImage {
        RgbaImage::new(self.width, self.height, self.pixels)
    }

    fn blend_pixel(&mut self, x: i32, y: i32, source: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = ((y as usize * self.width as usize) + x as usize) * 4;
        let Some(destination) = self.pixels.get_mut(index..index + 4) else {
            return;
        };
        let source_alpha = u32::from(source[3]);
        if source_alpha == 0 {
            return;
        }
        if source_alpha == 255 {
            destination.copy_from_slice(&source);
            return;
        }

        let destination_alpha = u32::from(destination[3]);
        let inverse_source = 255 - source_alpha;
        let alpha_numerator = source_alpha * 255 + destination_alpha * inverse_source;
        if alpha_numerator == 0 {
            destination.copy_from_slice(&[0; 4]);
            return;
        }
        for channel in 0..3 {
            let premultiplied = u32::from(source[channel]) * source_alpha * 255
                + u32::from(destination[channel]) * destination_alpha * inverse_source;
            destination[channel] = ((premultiplied + alpha_numerator / 2) / alpha_numerator) as u8;
        }
        destination[3] = ((alpha_numerator + 127) / 255) as u8;
    }

    pub(crate) fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: [u8; 4]) {
        if width <= 0 || height <= 0 {
            return;
        }
        let x0 = i64::from(x).clamp(0, i64::from(self.width));
        let y0 = i64::from(y).clamp(0, i64::from(self.height));
        let x1 = (i64::from(x) + i64::from(width)).clamp(0, i64::from(self.width));
        let y1 = (i64::from(y) + i64::from(height)).clamp(0, i64::from(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for py in y0..y1 {
            for px in x0..x1 {
                self.blend_pixel(px as i32, py as i32, color);
            }
        }
    }

    /// Draw an RGBA image centered in a bounded rectangle without scaling.
    ///
    /// Images larger than the rectangle are center-cropped. Callers that need
    /// fit behavior should render to the target bound before drawing.
    pub(crate) fn draw_image_centered(
        &mut self,
        image: &RgbaImage,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> bool {
        if width <= 0 || height <= 0 || !image.is_well_formed() {
            return false;
        }

        let image_width = i32::try_from(image.width).unwrap_or(i32::MAX);
        let image_height = i32::try_from(image.height).unwrap_or(i32::MAX);
        let copy_width = image_width.min(width);
        let copy_height = image_height.min(height);
        let source_x = (image_width - copy_width) / 2;
        let source_y = (image_height - copy_height) / 2;
        let destination_x = x + (width - copy_width) / 2;
        let destination_y = y + (height - copy_height) / 2;

        for row in 0..copy_height {
            for column in 0..copy_width {
                self.blend_pixel(
                    destination_x + column,
                    destination_y + row,
                    image.pixel((source_x + column) as u32, (source_y + row) as u32),
                );
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_text_clipped(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        max_width: i32,
        max_height: i32,
        color: [u8; 4],
        scale: u32,
    ) {
        if max_width <= 0 || max_height <= 0 || scale == 0 {
            return;
        }
        let scale = i32::try_from(scale).unwrap_or(i32::MAX);
        let cell_width = 6i32.saturating_mul(scale);
        if cell_width <= 0 {
            return;
        }
        let requested_chars = usize::try_from(max_width / cell_width).unwrap_or(0);
        // A malformed caller can provide an enormous clip rectangle, but no
        // glyphs beyond the bounded image can affect the result.
        let image_chars = (self.width as usize)
            .checked_div(cell_width as usize)
            .unwrap_or(0)
            .saturating_add(4);
        let max_chars = requested_chars.min(image_chars);
        let characters = truncated_chars(text, max_chars);
        let clip_x1 = i64::from(x) + i64::from(max_width);
        let clip_y1 = i64::from(y) + i64::from(max_height);

        for (index, character) in characters.into_iter().enumerate() {
            let glyph_x = i64::from(x) + index as i64 * i64::from(cell_width);
            let rows = glyph_rows(character);
            for (row, bits) in rows.into_iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) == 0 {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = glyph_x + i64::from(column * scale + sx);
                            let py = i64::from(y) + row as i64 * i64::from(scale) + i64::from(sy);
                            if px >= i64::from(x)
                                && py >= i64::from(y)
                                && px < clip_x1
                                && py < clip_y1
                            {
                                self.blend_pixel(px as i32, py as i32, color);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn truncated_chars(text: &str, max_chars: usize) -> Vec<char> {
    if max_chars == 0 {
        return Vec::new();
    }
    let mut characters: Vec<char> = text.chars().take(max_chars + 1).collect();
    if characters.len() <= max_chars {
        return characters;
    }
    if max_chars <= 3 {
        return vec!['.'; max_chars];
    }
    characters.truncate(max_chars - 3);
    characters.extend(['.', '.', '.']);
    characters
}

fn text_width(text: &str, scale: u32) -> i32 {
    let count = text.chars().count().min(i32::MAX as usize) as i32;
    count
        .saturating_mul(6)
        .saturating_mul(i32::try_from(scale).unwrap_or(i32::MAX))
}

fn glyph_rows(character: char) -> [u8; 7] {
    let character = if character.is_ascii_lowercase() {
        character.to_ascii_uppercase()
    } else {
        character
    };
    match character {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 4, 4, 8],
        ':' => [0, 4, 4, 0, 4, 4, 0],
        ';' => [0, 4, 4, 0, 4, 4, 8],
        '!' => [4, 4, 4, 4, 4, 0, 4],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '#' => [10, 31, 10, 10, 31, 10, 0],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '\\' => [16, 8, 8, 4, 2, 2, 1],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '{' => [3, 4, 4, 8, 4, 4, 3],
        '}' => [24, 4, 4, 2, 4, 4, 24],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '*' => [0, 17, 10, 31, 10, 17, 0],
        '\'' => [4, 4, 2, 0, 0, 0, 0],
        '"' => [10, 10, 5, 0, 0, 0, 0],
        '%' => [17, 2, 4, 8, 16, 17, 0],
        '&' => [12, 18, 20, 8, 21, 18, 13],
        '@' => [14, 17, 23, 21, 23, 16, 14],
        '|' => [4, 4, 4, 4, 4, 4, 4],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        '~' => [0, 0, 9, 22, 0, 0, 0],
        '^' => [4, 10, 17, 0, 0, 0, 0],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_raster_is_clipped_and_truncation_is_bounded() {
        let mut canvas = Canvas::new(4, 3, BACKGROUND).unwrap();
        let original_len = canvas.pixels.len();
        let extreme = "line\n\u{1f680}".repeat(20_000);
        canvas.draw_text_clipped(&extreme, -100, -100, i32::MAX, i32::MAX, PRIMARY_TEXT, 1);
        assert_eq!(canvas.pixels.len(), original_len);
        assert!(canvas.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));

        assert_eq!(
            truncated_chars("abcdefgh", 5),
            vec!['a', 'b', '.', '.', '.']
        );
        assert_eq!(truncated_chars("abcdefgh", 2), vec!['.', '.']);
    }

    #[test]
    fn ruler_interval_targets_a_readable_label_count() {
        assert_eq!(choose_tick_interval(1.0), 0.1);
        assert_eq!(choose_tick_interval(10.0), 1.0);
        assert_eq!(choose_tick_interval(120.0), 15.0);
    }
}
