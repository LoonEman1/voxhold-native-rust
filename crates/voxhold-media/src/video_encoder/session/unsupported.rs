//! Non-Windows answer of the hardware session.
//!
//! The publication path still works there in CPU mode; an explicit GPU choice
//! is refused with this message instead of pretending an encoder exists.

pub(crate) const MESSAGE: &str =
    "Аппаратное кодирование доступно только в Windows; выберите «Процессор».";
