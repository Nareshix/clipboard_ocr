use arboard::Clipboard;
use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;
use image::{ImageBuffer, RgbaImage};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::Command;
use std::time::Duration;

#[derive(Clone)]
struct OcrChar {
    text: char, // We now store the actual character Tesseract saw
    rect: egui::Rect,
}

#[derive(Clone)]
struct OcrWord {
    text: String,
    rect: egui::Rect,
    chars: Vec<OcrChar>,
}

struct ClipImage {
    rgba: Vec<u8>,
    size: [usize; 2],
    words: Vec<OcrWord>,
    texture: Option<egui::TextureHandle>,
}

enum Clip {
    Text(String),
    Image(ClipImage),
}

struct ClipboardManagerApp {
    clips: Vec<Clip>,
    search_query: String,
    receiver: Receiver<Clip>,
}

impl eframe::App for ClipboardManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(new_clip) = self.receiver.try_recv() {
            self.clips.push(new_clip);
            if self.clips.len() > 50 {
                self.clips.remove(0);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("📋 Smart Clipboard with True Character Highlighting");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("🔍 Search:");
                ui.text_edit_singleline(&mut self.search_query);
                if ui.button("Clear").clicked() {
                    self.search_query.clear();
                }
            });

            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                let search_lower = self.search_query.to_lowercase();
                let search_terms: Vec<&str> = search_lower.split_whitespace().collect();

                for clip in self.clips.iter_mut().rev() {
                    match clip {
                        Clip::Text(text) => {
                            if search_lower.is_empty() || text.to_lowercase().contains(&search_lower) {
                                render_text_card(ui, text, &self.search_query);
                            }
                        }
                        Clip::Image(img_clip) => {
                            let matches_search = search_terms.is_empty() || search_terms.iter().all(|&term| {
                                img_clip.words.iter().any(|w| {
                                    w.text.to_lowercase().contains(term)
                                })
                            });

                            if matches_search {
                                render_image_card(ui, img_clip, &search_terms);
                            }
                        }
                    }
                }
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([500.0, 700.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Smart OCR Clipboard",
        options,
        Box::new(|cc| {
            let (tx, rx) = unbounded();
            start_clipboard_poller(tx, cc.egui_ctx.clone());

            Box::new(ClipboardManagerApp {
                clips: Vec::new(),
                search_query: String::new(),
                receiver: rx,
            })
        }),
    )
}

fn start_clipboard_poller(sender: Sender<Clip>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut clipboard = Clipboard::new().expect("Failed to initialize clipboard");
        let mut last_text = String::new();
        let mut last_img_hash: u64 = 0;

        loop {
            if let Ok(text) = clipboard.get_text() {
                if !text.trim().is_empty() && text != last_text {
                    last_text = text.clone();
                    let _ = sender.send(Clip::Text(text));
                    ctx.request_repaint();
                }
            }

            if let Ok(img) = clipboard.get_image() {
                let hash = hash_bytes(&img.bytes);
                if hash != last_img_hash {
                    last_img_hash = hash;
                    let words = perform_ocr_on_image(&img);
                    let _ = sender.send(Clip::Image(ClipImage {
                        rgba: img.bytes.into_owned(),
                        size: [img.width, img.height],
                        words,
                        texture: None,
                    }));
                    ctx.request_repaint();
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn perform_ocr_on_image(img: &arboard::ImageData) -> Vec<OcrWord> {
    let buffer: RgbaImage = ImageBuffer::from_raw(img.width as u32, img.height as u32, img.bytes.to_vec())
        .expect("Failed to parse image buffer");

    let temp_img_path = std::env::temp_dir().join("clipboard_ocr_temp.png");
    buffer.save(&temp_img_path).ok();
    let img_h = img.height as f32;

    println!("[OCR] Running Tesseract (Two-Pass Parallel)...");

    let path_1 = temp_img_path.clone();
    let tsv_thread = std::thread::spawn(move || {
        Command::new("tesseract").arg(&path_1).arg("stdout").arg("tsv").output()
    });

    let path_2 = temp_img_path.clone();
    let mb_thread = std::thread::spawn(move || {
        Command::new("tesseract").arg(&path_2).arg("stdout").arg("makebox").output()
    });

    let tsv_output = tsv_thread.join().unwrap();
    let mb_output = mb_thread.join().unwrap();

    let mut words = Vec::new();
    let mut all_chars = Vec::new();

    // 1. Parse Character Boxes (Makebox)
    if let Ok(out) = mb_output {
        let mb_str = String::from_utf8_lossy(&out.stdout);
        for line in mb_str.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                // Grab the actual character recognized in this box!
                let char_text = parts[0].chars().next().unwrap_or(' ');

                let left = parts[1].parse::<f32>().unwrap_or(0.0);
                let bottom = parts[2].parse::<f32>().unwrap_or(0.0);
                let right = parts[3].parse::<f32>().unwrap_or(0.0);
                let top = parts[4].parse::<f32>().unwrap_or(0.0);

                let y_min = img_h - top;
                let y_max = img_h - bottom;

                all_chars.push(OcrChar {
                    text: char_text,
                    rect: egui::Rect::from_min_max(
                        egui::pos2(left, y_min),
                        egui::pos2(right, y_max),
                    ),
                });
            }
        }
    }

    // 2. Parse Word Boxes (TSV)
    if let Ok(out) = tsv_output {
        let tsv_str = String::from_utf8_lossy(&out.stdout);
        for line in tsv_str.lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() >= 12 {
                let conf = cols[10].parse::<f32>().unwrap_or(-1.0);
                if conf > 10.0 {
                    let text = cols[11].trim().to_string();
                    if !text.is_empty() {
                        let x = cols[6].parse::<f32>().unwrap_or(0.0);
                        let y = cols[7].parse::<f32>().unwrap_or(0.0);
                        let w = cols[8].parse::<f32>().unwrap_or(0.0);
                        let h = cols[9].parse::<f32>().unwrap_or(0.0);

                        words.push(OcrWord {
                            text,
                            rect: egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)),
                            chars: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    // 3. Map & Align Characters (The Magic Fix for Icons)
    for word in &mut words {
        let capture_box = word.rect.expand(4.0);
        let mut raw_chars = Vec::new();

        for ch in &all_chars {
            if capture_box.contains(ch.rect.center()) {
                raw_chars.push(ch.clone());
            }
        }
        raw_chars.sort_by(|a, b| a.rect.min.x.partial_cmp(&b.rect.min.x).unwrap());

        // We run a sequence alignment to ignore noise boxes (like the Rust Icon)
        let mut aligned_chars = Vec::new();
        let tsv_chars: Vec<char> = word.text.chars().collect();
        let mut mb_idx = 0;

        for &tc in &tsv_chars {
            let mut found = false;
            let tc_lower = tc.to_lowercase().next().unwrap_or(tc);

            // Scan forward in the raw characters to find the matching letter
            while mb_idx < raw_chars.len() {
                let mc_lower = raw_chars[mb_idx].text.to_lowercase().next().unwrap_or(raw_chars[mb_idx].text);

                // If the letters match, we've found the true bounding box for this letter!
                if mc_lower == tc_lower {
                    aligned_chars.push(raw_chars[mb_idx].clone());
                    mb_idx += 1;
                    found = true;
                    break;
                }
                mb_idx += 1; // Skip non-matching boxes (like the Rust icon!)
            }

            if !found {
                break; // Mismatch between passes
            }
        }

        // If we perfectly aligned every letter in the word...
        if aligned_chars.len() == tsv_chars.len() {
            word.chars = aligned_chars;

            // Fix the word's bounding box! By rebuilding it entirely from the matching
            // letters, we mathematically amputate the Rust icon out of the selection entirely!
            let min_x = word.chars.iter().map(|c| c.rect.min.x).fold(f32::INFINITY, f32::min);
            let max_x = word.chars.iter().map(|c| c.rect.max.x).fold(f32::NEG_INFINITY, f32::max);
            let min_y = word.chars.iter().map(|c| c.rect.min.y).fold(f32::INFINITY, f32::min);
            let max_y = word.chars.iter().map(|c| c.rect.max.y).fold(f32::NEG_INFINITY, f32::max);

            word.rect = egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y));
        }
    }

    println!("[OCR] Processed new image! Found {} words.", words.len());
    words
}

fn render_text_card(ui: &mut egui::Ui, text: &str, search: &str) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        if search.is_empty() {
            ui.label(text);
        } else {
            let mut job = egui::text::LayoutJob::default();
            let lower_text = text.to_lowercase();
            let lower_search = search.to_lowercase();
            let mut start = 0;

            while let Some(idx) = lower_text[start..].find(&lower_search) {
                let actual_idx = start + idx;
                if actual_idx > start {
                    job.append(&text[start..actual_idx], 0.0, egui::TextFormat::default());
                }
                job.append(
                    &text[actual_idx..actual_idx + search.len()],
                    0.0,
                    egui::TextFormat {
                        background: egui::Color32::YELLOW,
                        color: egui::Color32::BLACK,
                        ..Default::default()
                    },
                );
                start = actual_idx + search.len();
            }
            if start < text.len() {
                job.append(&text[start..], 0.0, egui::TextFormat::default());
            }
            ui.label(job);
        }
    });
    ui.add_space(4.0);
}

fn render_image_card(ui: &mut egui::Ui, img_clip: &mut ClipImage, search_terms: &[&str]) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        let texture = img_clip.texture.get_or_insert_with(|| {
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                img_clip.size,
                &img_clip.rgba,
            );
            ui.ctx().load_texture(
                "clipboard_image",
                color_image,
                egui::TextureOptions::LINEAR,
            )
        });

        let max_size = egui::vec2(ui.available_width(), 300.0);
        let mut ui_img_size = texture.size_vec2();
        if ui_img_size.x > max_size.x {
            ui_img_size = ui_img_size * (max_size.x / ui_img_size.x);
        }
        if ui_img_size.y > max_size.y {
            ui_img_size = ui_img_size * (max_size.y / ui_img_size.y);
        }

        let (rect, _response) = ui.allocate_exact_size(ui_img_size, egui::Sense::hover());
        ui.painter().image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        if !search_terms.is_empty() {
            let scale_x = ui_img_size.x / img_clip.size[0] as f32;
            let scale_y = ui_img_size.y / img_clip.size[1] as f32;

            for word in &img_clip.words {
                let w_lower = word.text.to_lowercase();

                for &term in search_terms {
                    let mut start_idx = 0;

                    while let Some(idx) = w_lower[start_idx..].find(term) {
                        let match_start = start_idx + idx;
                        let match_end = match_start + term.len();

                        let total_chars = word.text.chars().count();
                        let start_char_idx = w_lower[..match_start].chars().count();
                        let end_char_idx = w_lower[..match_end].chars().count();

                        let highlight_rect = if word.chars.len() == total_chars {
                            let start_box = &word.chars[start_char_idx];
                            let end_box = &word.chars[end_char_idx - 1];

                            let base_min_x = start_box.rect.min.x * scale_x;
                            let base_max_x = end_box.rect.max.x * scale_x;
                            let y_min = word.rect.min.y * scale_y;
                            let y_max = word.rect.max.y * scale_y;

                            egui::Rect::from_min_max(
                                rect.min + egui::vec2(base_min_x, y_min),
                                rect.min + egui::vec2(base_max_x, y_max),
                            )
                        } else {
                            // Even the fallback is better now, because the word.rect has been
                            // cleanly shrunken away from any icons!
                            let start_frac = start_char_idx as f32 / total_chars as f32;
                            let end_frac = end_char_idx as f32 / total_chars as f32;

                            let base_min_x = word.rect.min.x * scale_x;
                            let base_w = word.rect.width() * scale_x;
                            let y_min = word.rect.min.y * scale_y;
                            let y_max = word.rect.max.y * scale_y;

                            egui::Rect::from_min_max(
                                rect.min + egui::vec2(base_min_x + base_w * start_frac, y_min),
                                rect.min + egui::vec2(base_min_x + base_w * end_frac, y_max),
                            )
                        };

                        ui.painter().rect_filled(
                            highlight_rect,
                            2.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 0, 130),
                        );
                        ui.painter().rect_stroke(
                            highlight_rect,
                            1.0,
                            egui::Stroke::new(1.0, egui::Color32::YELLOW),
                        );

                        start_idx = match_end;
                    }
                }
            }
        }
    });
    ui.add_space(4.0);
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}