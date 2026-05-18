slint::include_modules!();

use lopdf::{Document, Object, ObjectId, Dictionary};
use std::path::Path;

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let ui_handle = ui.as_weak();

    ui.on_select_file({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .pick_file()
            {
                if let Some(ui) = ui_handle.upgrade() {
                    let path_str = path.to_string_lossy().into_owned();
                    ui.set_input_file(path_str.clone().into());
                    let _input_path = Path::new(&path_str);
                    
                    if let Ok(doc) = Document::load(&path) {
                        let pages = doc.get_pages();
                        if let Some(&first_page) = pages.values().next() {
                            if let Ok(Object::Dictionary(dict)) = doc.get_object(first_page) {
                                if let Ok(mb) = dict.get(b"MediaBox") {
                                    if let Ok(arr) = mb.as_array() {
                                        if arr.len() >= 4 {
                                            let w = get_float(&arr[2]) - get_float(&arr[0]);
                                            let h = get_float(&arr[3]) - get_float(&arr[1]);
                                            
                                            let w_mm = w * 25.4 / 72.0;
                                            let h_mm = h * 25.4 / 72.0;
                                            
                                            ui.set_first_page_size(format!("{:.1} x {:.1} mm ({:.0} x {:.0} pt)", w_mm, h_mm, w, h).into());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ui.set_status_message("File selected. Ready to generate.".into());
                }
            }
        }
    });

    ui.on_generate({
        let ui_handle = ui_handle.clone();
        move |pages_per_booklet_str, separate_files, last_page_back, flip_backs, rtl, output_mode| {
            if let Some(ui) = ui_handle.upgrade() {
                let input_file = ui.get_input_file().to_string();
                if input_file.is_empty() {
                    return;
                }
                
                let default_out = format!("{}_booklet.pdf", Path::new(&input_file).file_stem().unwrap_or_default().to_string_lossy());
                
                let save_path = if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .set_file_name(&default_out)
                    .save_file()
                {
                    path.to_string_lossy().into_owned()
                } else {
                    ui.set_status_message("Save cancelled.".into());
                    return;
                };
                
                ui.set_status_message("Processing...".into());
                
                let pages_per_booklet = if pages_per_booklet_str == "All in one" {
                    0
                } else {
                    pages_per_booklet_str.parse::<usize>().unwrap_or(0)
                };
                
                match process_pdf(
                    &input_file,
                    pages_per_booklet,
                    separate_files,
                    last_page_back,
                    flip_backs,
                    rtl,
                    output_mode.as_str(),
                    &save_path,
                ) {
                    Ok(msg) => ui.set_status_message(msg.into()),
                    Err(e) => ui.set_status_message(format!("Error: {}", e).into()),
                }
            }
        }
    });

    ui.run()
}

fn process_pdf(
    input_file: &str,
    mut pages_per_booklet: usize,
    separate_files: bool,
    last_page_back: bool,
    flip_backs: bool,
    rtl: bool,
    output_mode: &str,
    save_as: &str,
) -> Result<String, String> {
    let input_path = Path::new(input_file);
    let mut doc = Document::load(input_path).map_err(|e| format!("Failed to load PDF: {}", e))?;
    
    let original_pages = doc.get_pages(); // BTreeMap<u32, ObjectId>
    let pages: Vec<ObjectId> = original_pages.into_values().collect();
    
    if pages.is_empty() {
        return Err("PDF has no pages".to_string());
    }

    if pages_per_booklet == 0 {
        pages_per_booklet = ((pages.len() + 3) / 4) * 4;
    } else {
        pages_per_booklet = ((pages_per_booklet + 3) / 4) * 4;
    }
    
    if pages_per_booklet == 0 {
        return Err("Invalid pages per booklet".to_string());
    }

    // Prepare to make blank pages
    let mut mediabox = vec![
        lopdf::Object::Integer(0),
        lopdf::Object::Integer(0),
        lopdf::Object::Integer(595),
        lopdf::Object::Integer(842),
    ];
    if let Ok(Object::Dictionary(dict)) = doc.get_object(pages[0]) {
        if let Ok(mb) = dict.get(b"MediaBox") {
            if let Ok(arr) = mb.as_array() {
                mediabox = arr.clone();
            }
        }
    }

    let mut generated_files = Vec::new();
    let num_chunks = (pages.len() + pages_per_booklet - 1) / pages_per_booklet;
    
    // We will collect the reordered pages for all chunks to either save them
    // to separate files, or combine them into one file.
    let mut all_reordered_pages = Vec::new();

    for i in 0..num_chunks {
        let start = i * pages_per_booklet;
        let end = std::cmp::min(start + pages_per_booklet, pages.len());
        let mut chunk_pages = pages[start..end].to_vec();
        
        let is_last_chunk = i == num_chunks - 1;
        
        // Pad to pages_per_booklet
        while chunk_pages.len() < pages_per_booklet {
            let blank_id = create_blank_page(&mut doc, &mediabox);
            if is_last_chunk && last_page_back && !chunk_pages.is_empty() {
                // insert before the last page
                let last_idx = chunk_pages.len() - 1;
                chunk_pages.insert(last_idx, blank_id);
            } else {
                chunk_pages.push(blank_id);
            }
        }
        
        // Reorder for booklet
        let mut reordered = Vec::new();
        let n = chunk_pages.len();
        let half = n / 2;
        
        for j in 0..half {
            if j % 2 == 0 {
                // Front
                if rtl {
                    reordered.push(chunk_pages[j]);
                    reordered.push(chunk_pages[n - 1 - j]);
                } else {
                    reordered.push(chunk_pages[n - 1 - j]);
                    reordered.push(chunk_pages[j]);
                }
            } else {
                // Back
                if rtl {
                    reordered.push(chunk_pages[n - 1 - j]);
                    reordered.push(chunk_pages[j]);
                } else {
                    reordered.push(chunk_pages[j]);
                    reordered.push(chunk_pages[n - 1 - j]);
                }
            }
        }
        
        // Check imposition settings
        let impose = !output_mode.starts_with("Reorder");
        let mut tw = 842.0; // A4 Landscape
        let mut th = 595.0;
        if output_mode.contains("A3") {
            tw = 1190.0;
            th = 842.0;
        } else if output_mode.contains("Letter") {
            tw = 792.0;
            th = 612.0;
        }
        
        let mut final_pages_for_chunk = Vec::new();

        if impose {
            for (idx, pair) in reordered.chunks_exact(2).enumerate() {
                let p_left = pair[0];
                let p_right = pair[1];
                let f_left = create_form_xobject(&mut doc, p_left)?;
                let f_right = create_form_xobject(&mut doc, p_right)?;
                let is_back = idx % 2 != 0;
                let new_page_id = create_imposed_page(&mut doc, f_left, f_right, tw, th, is_back && flip_backs)?;
                final_pages_for_chunk.push(new_page_id);
            }
        } else {
            // Flip backs if requested (Reorder only)
            if flip_backs {
                for (idx, &page_id) in reordered.iter().enumerate() {
                    let pair_index = idx / 2;
                    if pair_index % 2 != 0 {
                        // rotate by 180
                        if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
                            let current_rot = match dict.get(b"Rotate") {
                                Ok(obj) => obj.as_i64().unwrap_or(0),
                                Err(_) => 0,
                            };
                            dict.set("Rotate", lopdf::Object::Integer((current_rot + 180) % 360));
                        }
                    }
                }
            }
            final_pages_for_chunk = reordered;
        }
        
        if separate_files {
            let mut chunk_doc = doc.clone();
            let parent_id = recreate_pages_tree(&mut chunk_doc, &final_pages_for_chunk)?;
            update_catalog(&mut chunk_doc, parent_id)?;
            chunk_doc.prune_objects(); // removes everything else
            
            let save_path = Path::new(save_as);
            let base_name = save_path.file_stem().unwrap_or_default().to_string_lossy();
            let out_name = format!("{}_{}.pdf", base_name, i + 1);
            let out_path = input_path.with_file_name(out_name);
            chunk_doc.save(&out_path).map_err(|e| format!("Save failed: {}", e))?;
            generated_files.push(out_path.to_string_lossy().into_owned());
        } else {
            all_reordered_pages.extend(final_pages_for_chunk);
        }
    }
    
    if !separate_files {
        let parent_id = recreate_pages_tree(&mut doc, &all_reordered_pages)?;
        update_catalog(&mut doc, parent_id)?;
        doc.prune_objects();
        
        let out_path = input_path.with_file_name(save_as);
        doc.save(&out_path).map_err(|e| format!("Save failed: {}", e))?;
        generated_files.push(out_path.to_string_lossy().into_owned());
    }
    
    Ok(format!("Success! Created {} file(s).", generated_files.len()))
}

fn create_blank_page(doc: &mut Document, mediabox: &[Object]) -> ObjectId {
    let mut dict = lopdf::Dictionary::new();
    dict.set("Type", lopdf::Object::Name(b"Page".to_vec()));
    dict.set("MediaBox", Object::Array(mediabox.to_vec()));
    doc.add_object(dict)
}

fn recreate_pages_tree(doc: &mut Document, page_ids: &[ObjectId]) -> Result<ObjectId, String> {
    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", lopdf::Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", lopdf::Object::Integer(page_ids.len() as i64));
    pages_dict.set("Kids", Object::Array(page_ids.iter().map(|&id| Object::Reference(id)).collect()));
    
    let pages_id = doc.add_object(pages_dict);
    
    for &page_id in page_ids {
        if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
            dict.set("Parent", Object::Reference(pages_id));
        }
    }
    
    Ok(pages_id)
}

fn update_catalog(doc: &mut Document, pages_id: ObjectId) -> Result<(), String> {
    let catalog_id = doc.trailer.get(b"Root")
        .map_err(|_| "No Root in trailer")?
        .as_reference()
        .map_err(|_| "Root is not a reference")?;
        
    if let Ok(Object::Dictionary(catalog)) = doc.get_object_mut(catalog_id) {
        catalog.set("Pages", Object::Reference(pages_id));
        Ok(())
    } else {
        Err("Catalog is not a dictionary".to_string())
    }
}

fn create_form_xobject(doc: &mut Document, page_id: ObjectId) -> Result<ObjectId, String> {
    let mut form_dict = Dictionary::new();
    form_dict.set("Type", lopdf::Object::Name(b"XObject".to_vec()));
    form_dict.set("Subtype", lopdf::Object::Name(b"Form".to_vec()));
    form_dict.set("FormType", lopdf::Object::Integer(1));
    
    let mut min_x = 0.0;
    let mut min_y = 0.0;
    
    if let Ok(Object::Dictionary(page_dict)) = doc.get_object(page_id) {
        if let Ok(bbox) = page_dict.get(b"MediaBox") {
            form_dict.set("BBox", bbox.clone());
            if let Ok(arr) = bbox.as_array() {
                if arr.len() >= 4 {
                    min_x = get_float(&arr[0]);
                    min_y = get_float(&arr[1]);
                }
            }
        } else {
            form_dict.set("BBox", Object::Array(vec![0.into(), 0.into(), 595.into(), 842.into()]));
        }
        if let Ok(res) = page_dict.get(b"Resources") {
            form_dict.set("Resources", res.clone());
        }
    }
    
    form_dict.set("Matrix", Object::Array(vec![
        1.into(), 0.into(), 0.into(), 1.into(),
        lopdf::Object::Real(-min_x as f32), lopdf::Object::Real(-min_y as f32)
    ]));
    
    let content_data = match doc.get_and_decode_page_content(page_id) {
        Ok(content) => content.encode().unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let form_stream = lopdf::Stream::new(form_dict, content_data);
    Ok(doc.add_object(form_stream))
}

fn create_imposed_page(
    doc: &mut Document, 
    f_left: ObjectId, 
    f_right: ObjectId, 
    tw: f64, 
    th: f64, 
    rotate_180: bool
) -> Result<ObjectId, String> {
    let half_w = tw / 2.0;
    
    let mut w_l = 595.0; let mut h_l = 842.0;
    if let Ok(Object::Stream(s)) = doc.get_object(f_left) {
        if let Ok(bbox) = s.dict.get(b"BBox") {
            if let Ok(arr) = bbox.as_array() {
                if arr.len() >= 4 {
                    w_l = get_float(&arr[2]) - get_float(&arr[0]);
                    h_l = get_float(&arr[3]) - get_float(&arr[1]);
                }
            }
        }
    }
    let s_l = (half_w / w_l).min(th / h_l);
    let tx_l = (half_w - w_l * s_l) / 2.0;
    let ty_l = (th - h_l * s_l) / 2.0;

    let mut w_r = 595.0; let mut h_r = 842.0;
    if let Ok(Object::Stream(s)) = doc.get_object(f_right) {
        if let Ok(bbox) = s.dict.get(b"BBox") {
            if let Ok(arr) = bbox.as_array() {
                if arr.len() >= 4 {
                    w_r = get_float(&arr[2]) - get_float(&arr[0]);
                    h_r = get_float(&arr[3]) - get_float(&arr[1]);
                }
            }
        }
    }
    let s_r = (half_w / w_r).min(th / h_r);
    let tx_r = (half_w - w_r * s_r) / 2.0;
    let ty_r = (th - h_r * s_r) / 2.0;

    let mut contents = String::new();
    contents.push_str("q\n");
    if rotate_180 {
        contents.push_str(&format!("-1 0 0 -1 {:.2} {:.2} cm\n", tw, th));
    }
    contents.push_str(&format!("q {:.4} 0 0 {:.4} {:.2} {:.2} cm /FLeft Do Q\n", s_l, s_l, tx_l, ty_l));
    contents.push_str(&format!("q {:.4} 0 0 {:.4} {:.2} {:.2} cm /FRight Do Q\n", s_r, s_r, tx_r + half_w, ty_r));
    contents.push_str("Q\n");

    let mut xobjs = Dictionary::new();
    xobjs.set("FLeft", Object::Reference(f_left));
    xobjs.set("FRight", Object::Reference(f_right));
    let mut res_dict = Dictionary::new();
    res_dict.set("XObject", Object::Dictionary(xobjs));

    let mut page_dict = Dictionary::new();
    page_dict.set("Type", lopdf::Object::Name(b"Page".to_vec()));
    page_dict.set("MediaBox", Object::Array(vec![0.into(), 0.into(), tw.into(), th.into()]));
    page_dict.set("Resources", Object::Dictionary(res_dict));
    
    let contents_stream = lopdf::Stream::new(Dictionary::new(), contents.into_bytes());
    let contents_id = doc.add_object(contents_stream);
    page_dict.set("Contents", Object::Reference(contents_id));
    
    Ok(doc.add_object(page_dict))
}

fn get_float(obj: &Object) -> f64 {
    match obj {
        Object::Real(f) => *f as f64,
        Object::Integer(i) => *i as f64,
        _ => 0.0,
    }
}
