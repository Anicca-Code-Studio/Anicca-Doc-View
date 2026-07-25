//! Debug: dump the structural summary of a PDF.
//! Run: cargo run --release --example pdf_info -- <path.pdf>

fn main() {
    let path = std::env::args().nth(1).expect("usage: pdf_info <file.pdf>");
    let bytes = std::fs::read(&path).expect("read file");

    println!("file: {} ({} bytes)", path, bytes.len());
    println!("is_pdf: {}", anicca_engine::pdf::is_pdf(&bytes));

    let doc = match anicca_engine::pdf::parse(&bytes) {
        Ok(d) => d,
        Err(e) => {
            println!("PARSE FAILED: {e}");
            return;
        }
    };

    println!("xref entries: {}", doc.file.entries.len());
    println!("recovered:    {}", doc.file.recovered);
    println!("encrypted:    {}", doc.file.trailer.contains_key("Encrypt"));
    // Cross-check the walked page list against the /Count the file declares.
    let declared = doc
        .file
        .catalog_ref()
        .and_then(|c| doc.file.oget(&c, "Pages"))
        .and_then(|p| doc.file.oget(&p, "Count"))
        .and_then(|c| c.as_i64());
    match declared {
        Some(n) if n as usize == doc.page_count() => {
            println!("pages:        {} (matches /Count)", doc.page_count())
        }
        Some(n) => println!(
            "pages:        {}  *** MISMATCH: /Count says {} ***",
            doc.page_count(),
            n
        ),
        None => println!("pages:        {} (no /Count declared)", doc.page_count()),
    }

    let fonts = doc.declared_fonts();
    if !fonts.is_empty() {
        println!("non-embedded fonts: {}", fonts.join(", "));
    }

    // Font inventory: subtype, embedded program, and encoding, so coverage
    // gaps show up without opening the file by hand.
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for page in &doc.pages {
        let table = match doc
            .file
            .oget(&page.resources, "Font")
            .and_then(|f| f.as_dict().cloned())
        {
            Some(t) => t,
            None => continue,
        };
        for key in table.keys() {
            let font = match doc.file.dget(&table, key) {
                Some(f) => f,
                None => continue,
            };
            let subtype = font.get("Subtype").and_then(|o| o.as_name()).unwrap_or("?").to_string();
            let base = font
                .get("BaseFont")
                .and_then(|o| o.as_name())
                .map(anicca_engine::pdf::strip_subset_tag)
                .unwrap_or_default();
            let holder = match doc.file.oget(&font, "DescendantFonts") {
                Some(d) => match d.as_array().and_then(|a| a.first().cloned()) {
                    Some(f) => doc.file.resolve(&f),
                    None => font.clone(),
                },
                None => font.clone(),
            };
            let desc_sub = holder.get("Subtype").and_then(|o| o.as_name()).unwrap_or("");
            let program = doc
                .file
                .oget(&holder, "FontDescriptor")
                .map(|d| {
                    ["FontFile", "FontFile2", "FontFile3"]
                        .iter()
                        .find(|k| d.get(**k).is_some())
                        .copied()
                        .unwrap_or("none")
                })
                .unwrap_or("no-descriptor");
            let enc = match doc.file.dget(font.as_dict().unwrap(), "Encoding") {
                Some(o) => o.as_name().unwrap_or("<dict>").to_string(),
                None => "-".to_string(),
            };
            let tu = if font.get("ToUnicode").is_some() { "ToUnicode" } else { "-" };
            seen.insert(format!(
                "  {subtype:<10} {desc_sub:<14} {program:<13} enc={enc:<18} {tu:<9} {base}"
            ));
        }
    }
    if !seen.is_empty() {
        println!("fonts:");
        for line in &seen {
            println!("{line}");
        }
    }

    for (i, page) in doc.pages.iter().enumerate() {
        let (w, h) = page.size();
        let content = anicca_engine::pdf::page::content_bytes(&doc.file, page);
        let res_keys: Vec<&str> = page
            .resources
            .as_dict()
            .map(|d| d.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        println!(
            "page {:>3}: {:>7.2} x {:>7.2} pt  rot={:>3}  media=[{:.1} {:.1} {:.1} {:.1}]  content={} B  res={:?}",
            i + 1,
            w,
            h,
            page.rotate,
            page.media[0],
            page.media[1],
            page.media[2],
            page.media[3],
            content.len(),
            res_keys
        );
        if i >= 19 && doc.page_count() > 20 {
            println!("... ({} more pages)", doc.page_count() - 20);
            break;
        }
    }
}
