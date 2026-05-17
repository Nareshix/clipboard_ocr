use arboard::Clipboard;
use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;
use image::{ImageBuffer, RgbaImage};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::Command;
use std::time::Duration;

#[derive(Clone)]
struct OcrWord {
    text: String,
    rect: egui::Rect,
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
            ui.heading("📋 Smart Clipboard with Tesseract OCR");
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
                // Split search into individual words
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

    println!("[OCR] Running Tesseract on new image...");

    let output = Command::new("tesseract")
        .arg(&temp_img_path)
        .arg("stdout")
        .arg("tsv")
        .output();

    let mut words = Vec::new();

    match output {
        Ok(out) => {
            if !out.status.success() {
                let err_msg = String::from_utf8_lossy(&out.stderr);
                println!("[OCR ERROR] Tesseract failed: {}", err_msg);
                return words;
            }

            let tsv = String::from_utf8_lossy(&out.stdout);
            for line in tsv.lines().skip(1) {
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
                                rect: egui::Rect::from_min_size(
                                    egui::pos2(x, y),
                                    egui::vec2(w, h),
                                ),
                            });
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("\n[CRITICAL ERROR] Failed to execute 'tesseract' command!");
            println!("Reason: {}", e);
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

                // For every word you type in the search bar
                for &term in search_terms {
                    let mut start_idx = 0;

                    // While the search term is found inside the OCR word
                    while let Some(idx) = w_lower[start_idx..].find(term) {
                        let match_start = start_idx + idx;
                        let match_end = match_start + term.len();

                        // --- SUB-WORD FRACTION MATH ---
                        // Count characters to find the mathematical ratio of the word
                        let total_chars = word.text.chars().count() as f32;
                        let start_char_idx = w_lower[..match_start].chars().count() as f32;
                        let end_char_idx = w_lower[..match_end].chars().count() as f32;

                        // e.g. If "gi" starts at char 1 out of 10, start_frac is 0.1
                        let start_frac = start_char_idx / total_chars;
                        let end_frac = end_char_idx / total_chars;

                        // Apply the UI scaling to the Tesseract bounding box
                        let base_min_x = word.rect.min.x * scale_x;
                        let base_w = word.rect.width() * scale_x;
                        let y_min = word.rect.min.y * scale_y;
                        let y_max = word.rect.max.y * scale_y;

                        // Slice the bounding box horizontally based on the fraction!
                        let highlight_rect = egui::Rect::from_min_max(
                            rect.min + egui::vec2(base_min_x + base_w * start_frac, y_min),
                            rect.min + egui::vec2(base_min_x + base_w * end_frac, y_max),
                        );

                        // Paint a glowing yellow box exactly over the sub-word
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