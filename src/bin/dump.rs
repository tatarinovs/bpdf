use lopdf::Document;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file = args.get(1).map(|s| s.as_str()).unwrap_or("test2.pdf");
    let doc = Document::load(file).unwrap();
    for (i, &page_id) in doc.get_pages().iter().take(2) {
        let streams = doc.get_page_contents(page_id);
        println!("Page {} has {} streams", i, streams.len());
        for id in streams {
            let s = doc.get_object(id).unwrap().as_stream().unwrap();
            let c = s
                .decompressed_content()
                .unwrap_or_else(|_| s.content.clone());
            let txt = String::from_utf8_lossy(&c);
            println!("Stream {}: {} bytes", id.0, txt.len());
            if txt.contains("F0") {
                println!("{}", txt);
            }
        }
    }
}
