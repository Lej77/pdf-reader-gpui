/// Wrap a type that provides a [`raw_window_handle::WindowHandle`] but doesn't
/// provide a [`raw_window_handle::DisplayHandle`] and makes it usable with
/// [`prompt_load_file`] and [`prompt_save_file`].
pub struct NoDisplayHandle<W>(pub W);
impl<W> raw_window_handle::HasWindowHandle for NoDisplayHandle<W>
where
    W: raw_window_handle::HasWindowHandle,
{
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        raw_window_handle::HasWindowHandle::window_handle(&self.0)
    }
}
impl<W> raw_window_handle::HasDisplayHandle for NoDisplayHandle<W>
where
    W: raw_window_handle::HasDisplayHandle,
{
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        // gpui panics if its `<gpui::Window as
        // HasDisplayHandle>::display_handle` method is called on Windows
        Err(raw_window_handle::HandleError::NotSupported)
    }
}

/// An object safe trait that can be used by
/// [`rfd::AsyncFileDialog::set_parent`].
pub trait DialogParent:
    raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle
{
}
impl<W> DialogParent for W where
    W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle
{
}

pub fn prompt_load_pdf_file(
    parent: Option<&dyn DialogParent>,
) -> impl Future<Output = Option<rfd::FileHandle>> + 'static {
    let mut builder = ::rfd::AsyncFileDialog::new()
        .add_filter("PDF file", &["pdf"])
        .add_filter("All files", &["*"])
        .set_title("Open PDF file");

    if let Some(parent) = parent {
        builder = builder.set_parent(&parent);
    }

    builder.pick_file()
}
