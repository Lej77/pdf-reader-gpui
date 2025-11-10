//! Utilities for using ELM like architecture where UI updates are done in response to messages.

use gpui::{AsyncWindowContext, Context, WeakEntity, Window};

pub trait Update<M>: Sized {
    fn update(&mut self, window: &mut Window, cx: &mut Context<Self>, msg: M);
}

pub struct MsgSender<T> {
    window_and_cx: AsyncWindowContext,
    weak: WeakEntity<T>,
}
impl<T: 'static> MsgSender<T> {
    pub fn new(window: AsyncWindowContext, weak: WeakEntity<T>) -> Self {
        Self {
            window_and_cx: window,
            weak,
        }
    }
    pub fn from_cx(window: &mut Window, cx: &mut Context<T>) -> Self {
        let window_and_cx = window.to_async(cx);
        let weak = cx.weak_entity();
        Self {
            window_and_cx,
            weak,
        }
    }

    pub fn spawn<R: 'static>(
        &self,
        f: impl AsyncFnOnce(&mut AsyncWindowContext, MsgSender<T>) -> R + 'static,
    ) -> gpui::Task<R> {
        let this = self.clone();
        self.window_and_cx
            .spawn(async move |window: &mut AsyncWindowContext| f(window, this).await)
    }

    pub fn send<M>(&mut self, msg: M)
    where
        T: Update<M>,
    {
        _ = self
            .window_and_cx
            .window_handle()
            .update(&mut self.window_and_cx, |_, window, cx| {
                let Some(view) = self.weak.upgrade() else {
                    return;
                };
                _ = view.update(cx, |view, cx| {
                    T::update(view, window, cx, msg);
                });
            });
    }
}
impl<T> Clone for MsgSender<T> {
    fn clone(&self) -> Self {
        Self {
            window_and_cx: self.window_and_cx.clone(),
            weak: self.weak.clone(),
        }
    }
}
