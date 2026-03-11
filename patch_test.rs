fn main() {
    let mut cleaned = String::from("  SELECT   *  FROM   nodes   ");
    let mut new_cleaned = String::with_capacity(cleaned.len());
    let mut first = true;
    for word in cleaned.split_whitespace() {
        if !first {
            new_cleaned.push(' ');
        }
        new_cleaned.push_str(word);
        first = false;
    }
    cleaned = new_cleaned;
    println!("{}", cleaned);
}
