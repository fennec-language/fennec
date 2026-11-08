//! Channels, as `std.concurrent` uses them through `std.intrinsics`: a channel is the runtime's,
//! by its number, and a message crosses it as one word. A number or a bool is that word. A block
//! goes as its address, which the runtime copies, so the sender keeps its own. A value held in
//! place, like a struct or a string's header, goes in a block made for the send.

use fennec_ir::{LocalId, Operand};

use crate::emit::Emit;
use crate::kinds::Kind;

impl Emit<'_, '_, '_> {
    /// `intrinsics.channel_*`: `channel_new`, `channel_send`, `channel_take` (what the IR makes
    /// of `channel_receive`), `channel_close` and `channel_free`.
    pub(crate) fn channel_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<(), String> {
        match (name, args) {
            ("intrinsics.channel_new", [size]) => {
                let (size, _) = self.operand(size)?;
                self.declare("declare i64 @fennec_channel_new(i64) nounwind");
                let made = self.temp();
                self.line(&format!("{made} = call i64 @fennec_channel_new(i64 {size})"));
                // A size below 1 fails.
                self.leave_if_failed();
                self.assign(dest.ok_or("a channel no one keeps")?, &made)
            }
            ("intrinsics.channel_new_every", [size]) => {
                let (size, _) = self.operand(size)?;
                self.declare("declare i64 @fennec_channel_new_every(i64) nounwind");
                let made = self.temp();
                self.line(&format!("{made} = call i64 @fennec_channel_new_every(i64 {size})"));
                // A size below 1 fails.
                self.leave_if_failed();
                self.assign(dest.ok_or("a channel no one keeps")?, &made)
            }
            ("intrinsics.channel_listen", [channel, drop_oldest]) => {
                let ((channel, _), (drop_oldest, _)) = (self.operand(channel)?, self.operand(drop_oldest)?);
                let flag = self.temp();
                self.line(&format!("{flag} = zext i1 {drop_oldest} to i64"));
                self.declare("declare i64 @fennec_channel_listen(i64, i64) nounwind");
                let listener = self.temp();
                self.line(&format!(
                    "{listener} = call i64 @fennec_channel_listen(i64 {channel}, i64 {flag})"
                ));
                self.assign(dest.ok_or("a listener no one keeps")?, &listener)
            }
            ("intrinsics.channel_close" | "intrinsics.channel_free", [channel]) => {
                let (channel, _) = self.operand(channel)?;
                let call = name.replace("intrinsics.", "fennec_");
                self.declare(&format!("declare void @{call}(i64) nounwind"));
                self.line(&format!("call void @{call}(i64 {channel})"));
                Ok(())
            }
            ("intrinsics.channel_send", [channel, message, timeout]) => {
                let (channel, _) = self.operand(channel)?;
                let (timeout, _) = self.operand(timeout)?;
                let (word, is_block, done) = self.message_word(message)?;
                self.declare("declare i64 @fennec_channel_send(i64, i64, i64, i64) nounwind");
                let sent = self.temp();
                self.line(&format!(
                    "{sent} = call i64 @fennec_channel_send(i64 {channel}, i64 {word}, i64 {is_block}, i64 {timeout})"
                ));
                // The runtime copied it, so a block made for the send is let go of.
                done(self);
                // A stop cutting the wait short fails it.
                self.leave_if_failed();
                match dest {
                    Some(dest) => self.assign(dest, &sent),
                    None => Ok(()),
                }
            }
            ("intrinsics.channel_take", [channel, timeout, Operand::Local(why)]) => {
                let dest = dest.ok_or("a message no one keeps")?;
                let (channel, _) = self.operand(channel)?;
                let (timeout, _) = self.operand(timeout)?;
                let word_at = self.place_for(&Kind::Int);
                self.declare("declare i64 @fennec_channel_receive(i64, i64, ptr) nounwind");
                let code = self.temp();
                self.line(&format!(
                    "{code} = call i64 @fennec_channel_receive(i64 {channel}, i64 {timeout}, ptr {word_at})"
                ));
                self.leave_if_failed();
                self.assign(*why, &code)?;
                let (got, take, nothing, done) =
                    (self.temp(), self.label("message"), self.label("no_message"), self.label("received"));
                self.line(&format!("{got} = icmp eq i64 {code}, 0"));
                self.line(&format!("br i1 {got}, label %{take}, label %{nothing}"));
                self.start_block(&take);
                let word = self.temp();
                self.line(&format!("{word} = load i64, ptr {word_at}"));
                self.message_from_word(dest, &word)?;
                self.line(&format!("br label %{done}"));
                self.start_block(&nothing);
                // Nothing came, so the message is left empty: the IR makes it `None`.
                let kind = self.kind_of(dest)?.clone();
                let slot = self.slot(dest);
                let zero = self.zero_line(&kind, &slot);
                self.line(&zero);
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                Ok(())
            }
            _ => Err(format!("it calls `{name}` in a way this backend doesn't take yet")),
        }
    }

    /// A message as one word, whether that word is a block's address, and what to do once it's
    /// sent: let go of a block made for it.
    #[expect(clippy::type_complexity, reason = "what's done after the send is the caller's to run")]
    fn message_word(
        &mut self,
        message: &Operand,
    ) -> Result<(String, &'static str, Box<dyn FnOnce(&mut Self)>), String> {
        let (value, kind) = self.operand(message)?;
        let nothing: Box<dyn FnOnce(&mut Self)> = Box::new(|_| {});
        Ok(match kind {
            Kind::Int | Kind::U64 => (value, "0", nothing),
            Kind::Bool => {
                let word = self.temp();
                self.line(&format!("{word} = zext i1 {value} to i64"));
                (word, "0", nothing)
            }
            Kind::Float | Kind::F32 => {
                let word = self.temp();
                self.line(&format!("{word} = bitcast double {value} to i64"));
                (word, "0", nothing)
            }
            Kind::Str if self.headers() => {
                let (block, owned) = self.text_block(&value);
                let word = self.temp();
                self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
                (word, "1", Box::new(move |emit: &mut Self| emit.let_go_text_block(&block, &owned)))
            }
            Kind::Struct(id) | Kind::Enum(id) => {
                let block = self.new_block(id)?;
                let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                self.store_field(&kind, &value, &fields, "");
                self.retain_inline(&kind, &fields);
                let word = self.temp();
                self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
                (word, "1", Box::new(move |emit: &mut Self| emit.release(&block)))
            }
            Kind::List(_) | Kind::Str | Kind::Bytes | Kind::Boxed(_) | Kind::Dict(..) => {
                let word = self.temp();
                self.line(&format!("{word} = ptrtoint ptr {value} to i64"));
                (word, "1", nothing)
            }
            other => {
                return Err(format!(
                    "it sends a {other:?} on a channel, which this backend doesn't take yet"
                ));
            }
        })
    }

    /// A message received as one word, as the local `dest`, which now owns what it holds.
    fn message_from_word(&mut self, dest: LocalId, word: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        match kind {
            Kind::Int | Kind::U64 => self.assign(dest, word),
            Kind::Bool => {
                let flag = self.temp();
                self.line(&format!("{flag} = trunc i64 {word} to i1"));
                self.assign(dest, &flag)
            }
            Kind::Float | Kind::F32 => {
                let number = self.temp();
                self.line(&format!("{number} = bitcast i64 {word} to double"));
                self.assign(dest, &number)
            }
            Kind::Str if self.headers() => {
                let block = self.temp();
                self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
                let header = self.text_from_block(&block);
                self.assign(dest, &header)
            }
            Kind::Struct(_) | Kind::Enum(_) => {
                let block = self.temp();
                self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
                let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                self.read_into(dest, &kind, &fields, "")?;
                self.release(&block);
                Ok(())
            }
            Kind::List(_) | Kind::Str | Kind::Bytes | Kind::Boxed(_) | Kind::Dict(..) => {
                let block = self.temp();
                self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
                self.assign(dest, &block)
            }
            other => {
                Err(format!("it receives a {other:?} from a channel, which this backend doesn't take yet"))
            }
        }
    }
}
