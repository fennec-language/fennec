//! The `postgres` target: `std.postgres`'s `PgReader`, built natively, fed any bytes as a
//! server's, all at once and in small pieces. Both ways have to give the same messages, and those
//! have to be what a reader here makes of the bytes as Postgres's protocol (3.0) writes them: each
//! message whole, with nothing left over in it and no count below zero, or a failure. A message
//! over 30,000 bytes fails from its header unless its type is on libpq's list of long ones.

/// The program that reads the input as a server's messages, all at once and then a few bytes at
/// a time, printing a line for each message, then `fail` or `end`.
pub(crate) const READER: &str = "\
from std.postgres import PgMessage, PgReader

def shown(message: PgMessage) -> str:
    match message:
        case .AuthOk:
            return \"auth 0\"
        case .AuthCleartext:
            return \"auth 3\"
        case .AuthSasl(mechanisms=mechanisms):
            return f\"sasl {len(mechanisms)}\"
        case .AuthSaslContinue(data=data):
            return f\"auth 11 {len(data)}\"
        case .AuthSaslFinal(data=data):
            return f\"auth 12 {len(data)}\"
        case .AuthOther(code=code):
            return f\"auth {code}\"
        case .Setting(name=name, value=value):
            return f\"setting {len(name)} {len(value)}\"
        case .BackendKey(process=process, secret=secret):
            return f\"key {process} {secret}\"
        case .Ready(status=status):
            return f\"ready {len(status)}\"
        case .Failed(fields=fields):
            return f\"failed {len(fields)}\"
        case .Notice(fields=fields):
            return f\"notice {len(fields)}\"
        case .Columns(names=names, types=types):
            return f\"columns {len(names)} {len(types)}\"
        case .Row(cells=cells, present=present):
            return f\"row {len(cells)} {len(present)}\"
        case .Done(tag=tag):
            return f\"done {len(tag)}\"
        case .ParseComplete:
            return \"other 49\"
        case .BindComplete:
            return \"other 50\"
        case .NoData:
            return \"other 110\"
        case .EmptyQuery:
            return \"other 73\"
        case .Other(kind=kind):
            return f\"other {kind}\"

def read_all(ref reader: PgReader) -> list[str]:
    mut said = list[str]()
    while len(said) < 10000:
        message = reader.next() except .Constraint() | .Busy() | .Column() | .Failed():
            said.append(\"fail\")
            return said
        if message is None:
            said.append(\"end\")
            return said
        said.append(shown(message))
    return said

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    data = fs.read_bytes(args[0])?
    mut whole = PgReader()
    whole.feed(data)
    for line in read_all(ref whole):
        stdout.print(line)
    stdout.print(\"--\")
    mut pieces = PgReader()
    mut said = list[str]()
    mut at = 0
    mut size = 1
    while at < len(data):
        end = at + size if at + size < len(data) else len(data)
        pieces.feed(data[at:end])
        at = end
        size = size % 7 + 1
        mut more = True
        while more:
            message = pieces.next() except .Constraint() | .Busy() | .Column() | .Failed():
                said.append(\"fail\")
                at = len(data) + 1
                break
            if message is None:
                more = False
            else:
                said.append(shown(message))
    if at == len(data) + 1:
        for line in said:
            stdout.print(line)
    else:
        for line in said:
            stdout.print(line)
        stdout.print(\"end\")
";

/// The longest message `std.postgres` takes.
const LONGEST: i64 = 1 << 30;

/// The longest a message may be unless its type is one of `LONG_KINDS`, as libpq has it.
const LONGEST_SHORT: i64 = 30_000;

/// The types of message that may be longer than `LONGEST_SHORT`: libpq's `VALID_LONG_MESSAGE_TYPE`.
const LONG_KINDS: [u8; 8] = [b'd', b'D', b'E', b'V', b'N', b'A', b'T', b't'];

/// A message's body, read from its start.
struct Body<'a> {
    data: &'a [u8],
    at: usize,
}

/// Why reading stopped: the bytes break the protocol, or it's not certain what they should give.
#[derive(Debug, PartialEq, Eq)]
enum Broken {
    Fails,
    Unpromised,
}

impl Body<'_> {
    fn take(&mut self, size: usize) -> Result<&[u8], Broken> {
        let taken = self.data.get(self.at..self.at + size).ok_or(Broken::Fails)?;
        self.at += size;
        Ok(taken)
    }

    fn int16(&mut self) -> Result<i64, Broken> {
        let bytes = self.take(2)?;
        Ok(i64::from(i16::from_be_bytes([*bytes.first().unwrap_or(&0), *bytes.get(1).unwrap_or(&0)])))
    }

    fn int32(&mut self) -> Result<i64, Broken> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| Broken::Fails)?;
        Ok(i64::from(i32::from_be_bytes(bytes)))
    }

    fn text(&mut self) -> Result<String, Broken> {
        let rest = self.data.get(self.at..).unwrap_or_default();
        let end = rest.iter().position(|byte| *byte == 0).ok_or(Broken::Fails)?;
        let text = std::str::from_utf8(rest.get(..end).unwrap_or_default()).map_err(|_| Broken::Fails)?;
        self.at += end + 1;
        Ok(text.to_owned())
    }

    fn fields(&mut self) -> Result<usize, Broken> {
        let mut codes = Vec::new();
        while let Some(code) = self.data.get(self.at).copied().filter(|code| *code != 0) {
            if !code.is_ascii() {
                return Err(Broken::Unpromised);
            }
            self.at += 1;
            self.text()?;
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        // The zero byte that ends them.
        self.take(1)?;
        Ok(codes.len())
    }

    /// Fails unless the whole body has been read.
    fn done(&self) -> Result<(), Broken> {
        if self.at == self.data.len() { Ok(()) } else { Err(Broken::Fails) }
    }
}

/// What a message of type `kind` with `body` should print as.
fn message(kind: u8, data: &[u8]) -> Result<String, Broken> {
    let mut body = Body { data, at: 0 };
    let shown = match kind {
        b'R' => match body.int32()? {
            10 => {
                let mut count = 0;
                while !body.text()?.is_empty() {
                    count += 1;
                }
                format!("sasl {count}")
            }
            code @ (11 | 12) => format!("auth {code} {}", data.len().saturating_sub(4)),
            code @ (0 | 3) => format!("auth {code}"),
            // A way of signing in this client doesn't speak may carry anything after its code.
            code => {
                body.at = data.len();
                format!("auth {code}")
            }
        },
        b'S' => format!("setting {} {}", body.text()?.chars().count(), body.text()?.chars().count()),
        b'K' => format!("key {} {}", body.int32()?, body.int32()?),
        b'Z' => match data {
            [status] if status.is_ascii() => {
                body.at = 1;
                "ready 1".to_owned()
            }
            [_] => return Err(Broken::Unpromised),
            _ => return Err(Broken::Fails),
        },
        b'E' => format!("failed {}", body.fields()?),
        b'N' => format!("notice {}", body.fields()?),
        b'T' => {
            let count = body.int16()?;
            if count < 0 {
                return Err(Broken::Fails);
            }
            for _ in 0..count {
                body.text()?;
                body.take(18)?;
            }
            format!("columns {count} {count}")
        }
        b'D' => {
            let count = body.int16()?;
            if count < 0 {
                return Err(Broken::Fails);
            }
            for _ in 0..count {
                let size = body.int32()?;
                if size < -1 {
                    return Err(Broken::Fails);
                }
                body.take(usize::try_from(size).unwrap_or(0))?;
            }
            format!("row {count} {count}")
        }
        b'C' => format!("done {}", body.text()?.chars().count()),
        // These have nothing in them.
        b'1' | b'2' | b'n' | b'I' => {
            body.done()?;
            format!("other {kind}")
        }
        _ => return Ok(format!("other {kind}")),
    };
    if !matches!((kind, shown.as_str()), (b'R', s) if s.starts_with("auth 11") || s.starts_with("auth 12")) {
        body.done()?;
    }
    Ok(shown)
}

/// The lines the reader should print for `input`, or where they stop being certain.
fn wanted(input: &[u8]) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut at = 0;
    loop {
        let Some(head) = input.get(at..at + 5) else {
            lines.push("end".to_owned());
            return (lines, true);
        };
        let (kind, size) = (head.first().copied().unwrap_or(0), head.get(1..5).unwrap_or_default());
        let size = i64::from(i32::from_be_bytes(size.try_into().unwrap_or([0; 4])));
        if !(4..=LONGEST).contains(&size) || (size > LONGEST_SHORT && !LONG_KINDS.contains(&kind)) {
            lines.push("fail".to_owned());
            return (lines, true);
        }
        let end = at + 1 + usize::try_from(size).unwrap_or(usize::MAX);
        let Some(data) = input.get(at + 5..end) else {
            lines.push("end".to_owned());
            return (lines, true);
        };
        match message(kind, data) {
            Ok(shown) => lines.push(shown),
            Err(Broken::Fails) => {
                lines.push("fail".to_owned());
                return (lines, true);
            }
            Err(Broken::Unpromised) => return (lines, false),
        }
        at = end;
    }
}

/// The target: both ways of feeding the reader agree, and give what the protocol says.
pub(crate) fn postgres(input: &[u8]) -> Result<(), String> {
    let Some(printed) = crate::llvm::read_natively("postgres", input)? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let Some((whole, pieces)) = printed.split_once("--\n") else {
        return Err(format!("the reader printed something else:\n{printed}"));
    };
    let (whole, pieces): (Vec<&str>, Vec<&str>) = (whole.lines().collect(), pieces.lines().collect());
    if whole != pieces {
        return Err(format!("{input:?} read whole gives {whole:?}, but fed in pieces {pieces:?}"));
    }
    let (wanted, certain) = wanted(input);
    let wanted: Vec<&str> = wanted.iter().map(String::as_str).collect();
    let same = if certain { whole == wanted } else { whole.get(..wanted.len()) == Some(&wanted[..]) };
    if !same {
        return Err(format!("{input:?} reads as {whole:?}, but the protocol says {wanted:?}"));
    }
    Ok(())
}
