//! A channel's message can't be optional. A closed channel answers `receive` with `None`, so a
//! `None` sent would read as the channel closing, and `T?` of an optional `T` is that same `T?`:
//! a message of `int?` would be taken as an `int`. Refused where a channel's message is chosen,
//! at std's `channel[T]` and at a `launch` task's `Inbox[T]`, with the way to send one anyway.

use fennec_syntax::{Diagnostic, Span};

use crate::function::FnCx;
use crate::types::Type;

impl FnCx<'_> {
    /// Looks at what a channel made or a task's inbox takes, `message`, once the function's types
    /// are worked out.
    pub(crate) fn note_channel_message(&mut self, message: Type, span: Span) {
        self.pending_messages.push((message, span));
    }

    /// Refuses each channel message noted that settled to an optional.
    pub(crate) fn check_pending_messages(&mut self) {
        for (message, span) in std::mem::take(&mut self.pending_messages) {
            let settled = self.checker.settled(&message);
            if matches!(settled, Type::Optional(_) | Type::None) {
                self.error(optional_message(&settled, span));
            }
        }
    }
}

/// The message of a channel or an inbox whose message type is optional.
fn optional_message(ty: &Type, span: Span) -> Diagnostic {
    let message = format!(
        "a channel's message can't be optional, since `None` is how a closed channel answers, and this one is `{ty}`"
    );
    let help = format!("send a struct holding the `{ty}`, or an enum with a case for nothing");
    Diagnostic::new(message, span).with_help(help)
}
