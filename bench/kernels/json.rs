//! JSON, the twin of `json.fen`.

enum Json {
    Null,
    Flag(bool),
    Number(i64),
    Text(String),
    Items(Vec<Json>),
    Fields(Vec<String>, Vec<Json>),
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn skip_space(&mut self) {
        while self.at < self.text.len() && self.text[self.at] == b' ' {
            self.at += 1;
        }
    }

    fn expect_text(&mut self) -> String {
        self.at += 1;
        let start = self.at;
        while self.text[self.at] != b'"' {
            self.at += 1;
        }
        let found = String::from_utf8_lossy(&self.text[start..self.at]).into_owned();
        self.at += 1;
        found
    }

    fn number(&mut self) -> i64 {
        let mut value = 0;
        while self.at < self.text.len() && self.text[self.at].is_ascii_digit() {
            value = value * 10 + i64::from(self.text[self.at] - b'0');
            self.at += 1;
        }
        value
    }

    fn items(&mut self) -> Json {
        self.at += 1;
        let mut values = Vec::new();
        self.skip_space();
        while self.text[self.at] != b']' {
            values.push(self.value());
            self.skip_space();
            if self.text[self.at] == b',' {
                self.at += 1;
                self.skip_space();
            }
        }
        self.at += 1;
        Json::Items(values)
    }

    fn fields(&mut self) -> Json {
        self.at += 1;
        let (mut names, mut values) = (Vec::new(), Vec::new());
        self.skip_space();
        while self.text[self.at] != b'}' {
            names.push(self.expect_text());
            self.skip_space();
            self.at += 1;
            values.push(self.value());
            self.skip_space();
            if self.text[self.at] == b',' {
                self.at += 1;
                self.skip_space();
            }
        }
        self.at += 1;
        Json::Fields(names, values)
    }

    fn value(&mut self) -> Json {
        self.skip_space();
        match self.text[self.at] {
            b'{' => self.fields(),
            b'[' => self.items(),
            b'"' => Json::Text(self.expect_text()),
            b't' => {
                self.at += 4;
                Json::Flag(true)
            }
            b'f' => {
                self.at += 5;
                Json::Flag(false)
            }
            b'n' => {
                self.at += 4;
                Json::Null
            }
            _ => Json::Number(self.number()),
        }
    }
}

fn printed(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Flag(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Number(number) => out.push_str(&number.to_string()),
        Json::Text(text) => {
            out.push('"');
            out.push_str(text);
            out.push('"');
        }
        Json::Items(values) => {
            out.push('[');
            for (at, item) in values.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                printed(item, out);
            }
            out.push(']');
        }
        Json::Fields(names, values) => {
            out.push('{');
            for (at, name) in names.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(name);
                out.push_str("\":");
                printed(&values[at], out);
            }
            out.push('}');
        }
    }
}

fn document(records: i64) -> String {
    let mut text = String::from("[");
    for i in 0..records {
        if i > 0 {
            text.push_str(", ");
        }
        let active = if i % 2 == 0 { "true" } else { "false" };
        text.push_str(&format!("{{\"id\": {i}, \"name\": \"fox{i}\", \"active\": {active}, \"parent\": null, "));
        text.push_str(&format!(
            "\"tags\": [\"a\", \"b\", {}], \"at\": {{\"x\": {}, \"y\": {}}}}}",
            i % 7,
            i * 3,
            i * 5
        ));
    }
    text.push(']');
    text
}

fn main() {
    let text = document(std::hint::black_box(300));
    let mut total = 0;
    for _ in 0..8 {
        let mut reader = Reader { text: text.as_bytes(), at: 0 };
        let value = reader.value();
        let mut out = String::new();
        printed(&value, &mut out);
        total += out.len();
    }
    println!("{} {total}", text.len());
}
