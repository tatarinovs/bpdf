fn main() {
    let doc = lopdf::Document::load("test4.pdf").unwrap();
    for (page_number, page_id) in doc.get_pages() {
        println!("Page {}", page_number);
        let content_data = doc.get_page_content(page_id);
        println!("Content data length: {}", content_data.len());
        let content = lopdf::content::Content::decode(&content_data).unwrap();
        println!("Operations count: {}", content.operations.len());
    }
}
