// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use gtk::{gio, glib, prelude::*};
use solstone_linux::{
    dbus_client::ControlClient,
    import::{ImportDocument, ImportRow, RemoveConfirmation},
};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

#[derive(Clone)]
pub struct ImportPage {
    pub widget: gtk::Box,
    choose: gtk::Button,
    addition: gtk::Label,
    notice: gtk::Box,
    error: gtk::Label,
    recent: gtk::Label,
    recent_heading: gtk::Label,
    device_heading: gtk::Label,
    store_down: Rc<std::cell::Cell<bool>>,
    rows: gtk::Box,
    model: Rc<RefCell<Option<String>>>,
    chooser: Rc<RefCell<Option<gtk::FileChooserNative>>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Start);
    label.add_css_class("sol-quiet");
    label
}

fn section(text: &str) -> gtk::Label {
    let label = label(text);
    label.add_css_class("sol-section");
    label.add_css_class("sol-rule");
    label
}

fn ok(answer: Result<String, solstone_linux::dbus_client::ControlClientError>) -> bool {
    answer
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| v["ok"] == true)
}

pub fn file_paths(files: impl IntoIterator<Item = gio::File>) -> Vec<String> {
    files
        .into_iter()
        .map(|file| {
            file.path()
                .unwrap_or_else(|| PathBuf::from(file.uri().as_str()))
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

pub fn drop_paths(value: &glib::Value) -> Option<Vec<String>> {
    if let Ok(list) = value.get::<gtk::gdk::FileList>() {
        return Some(file_paths(list.files()));
    }
    value.get::<gio::File>().ok().map(|file| file_paths([file]))
}

impl ImportPage {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let choose = gtk::Button::with_label("choose files…");
        choose.add_css_class("sol-action");
        choose.set_halign(gtk::Align::Start);
        let addition = label("");
        addition.set_visible(false);
        let error = label("");
        error.add_css_class("sol-attn");
        error.set_visible(false);
        let notice = gtk::Box::new(gtk::Orientation::Vertical, 4);
        notice.append(&label(
            "something you were adding didn't finish saving. add it again.",
        ));
        let dismiss = gtk::Button::with_label("ok");
        dismiss.set_halign(gtk::Align::Start);
        notice.append(&dismiss);
        notice.set_visible(false);
        let recent = label("checking your journal.");
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 12);
        widget.append(&choose);
        widget.append(&addition);
        widget.append(&error);
        widget.append(&notice);
        let recent_heading = section("recent");
        widget.append(&recent_heading);
        widget.append(&recent);
        let device_heading = section("on this device");
        widget.append(&device_heading);
        widget.append(&rows);
        let page = Self {
            widget,
            choose,
            addition,
            notice,
            error,
            recent,
            recent_heading,
            device_heading,
            store_down: Rc::default(),
            rows,
            model: Rc::default(),
            chooser: Rc::default(),
        };
        let picked = page.clone();
        page.choose.connect_clicked(move |button| {
            if picked.chooser.borrow().is_some() {
                return;
            }
            let Some(window) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let chooser = gtk::FileChooserNative::new(
                Some("choose files…"),
                Some(&window),
                gtk::FileChooserAction::Open,
                Some("choose files…"),
                Some("cancel"),
            );
            chooser.set_select_multiple(true);
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("files"));
            for mime in [
                "application/pdf",
                "audio/*",
                "image/png",
                "image/jpeg",
                "image/gif",
                "image/webp",
                "text/plain",
            ] {
                filter.add_mime_type(mime);
            }
            chooser.add_filter(&filter);
            let page = picked.clone();
            let chooser_for_close = picked.chooser.clone();
            chooser.connect_response(move |chooser, response| {
                if response == gtk::ResponseType::Accept {
                    let files = chooser.files();
                    let paths = file_paths(
                        (0..files.n_items())
                            .filter_map(|i| files.item(i).and_downcast::<gio::File>()),
                    );
                    page.add(paths);
                }
                chooser.destroy();
                chooser_for_close.borrow_mut().take();
            });
            chooser.show();
            *picked.chooser.borrow_mut() = Some(chooser);
        });
        let page_dismiss = page.clone();
        dismiss.connect_clicked(move |_| {
            page_dismiss.clear_step();
            let page = page_dismiss.clone();
            glib::MainContext::default().spawn_local(async move {
                let success = match ControlClient::session().await {
                    Ok(client) => {
                        let success = client.import_dismiss_interrupted().await.is_ok();
                        page.reload(&client).await;
                        success
                    }
                    Err(_) => false,
                };
                if !success {
                    page.fail(solstone_linux::import::STEP_ERROR);
                } else {
                    page.clear_step();
                }
            });
        });
        page
    }

    fn fail(&self, text: &str) {
        self.error.set_text(if self.store_down.get() {
            solstone_linux::import::STORE_ERROR
        } else {
            text
        });
        self.error.set_visible(true);
    }

    fn clear_step(&self) {
        if !self.store_down.get() {
            self.error.set_text("");
            self.error.set_visible(false);
        }
    }

    async fn reload(&self, client: &ControlClient) {
        if let Ok(raw) = client.import_state().await
            && let Ok(document) = serde_json::from_str::<ImportDocument>(&raw)
        {
            self.refresh(&document);
        }
    }

    pub fn add(&self, paths: Vec<String>) {
        if self.store_down.get() {
            self.fail(solstone_linux::import::STORE_ERROR);
            return;
        }
        if paths.is_empty() {
            return;
        }
        self.choose.set_sensitive(false);
        self.clear_step();
        let page = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let success = match ControlClient::session().await {
                Ok(client) => {
                    let success = ok(client.import_files(paths).await);
                    page.reload(&client).await;
                    success
                }
                Err(_) => false,
            };
            if !success {
                page.fail(solstone_linux::import::STEP_ERROR);
            }
            if success {
                page.clear_step();
            }
            page.choose.set_sensitive(!page.store_down.get());
        });
    }

    pub fn refresh(&self, document: &ImportDocument) {
        let encoded = serde_json::to_string(document).expect("import document serializes");
        if self.model.borrow().as_ref() == Some(&encoded) {
            return;
        }
        *self.model.borrow_mut() = Some(encoded);
        self.choose
            .set_sensitive(!document.adding && !document.store_error);
        self.addition
            .set_text(document.addition.as_deref().unwrap_or(""));
        self.addition
            .set_visible(document.addition.is_some() && !document.store_error);
        let was_down = self.store_down.replace(document.store_error);
        if was_down && !document.store_error {
            self.clear_step();
        }
        self.notice
            .set_visible(document.interrupted && !document.store_error);
        self.recent.set_visible(!document.store_error);
        self.recent_heading.set_visible(!document.store_error);
        self.device_heading.set_visible(!document.store_error);
        self.rows.set_visible(!document.store_error);
        if document.store_error {
            self.fail(solstone_linux::import::STORE_ERROR);
        }
        self.recent.set_text(&document.recent);
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        for row in &document.rows {
            let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let name = label(&row.name);
            name.add_css_class("sol-ink");
            box_.append(&name);
            let reason = label(row.reason.as_deref().unwrap_or(&row.state));
            box_.append(&reason);
            if row.can_remove || row.can_retry {
                let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                if row.can_retry {
                    let retry = gtk::Button::with_label("try now");
                    actions.append(&retry);
                    let page = self.clone();
                    let id = row.id.clone();
                    retry.connect_clicked(move |button| {
                        page.clear_step();
                        button.set_sensitive(false);
                        let page = page.clone();
                        let id = id.clone();
                        let button = button.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let success = match ControlClient::session().await {
                                Ok(client) => {
                                    let success = ok(client.import_retry(&id).await);
                                    page.reload(&client).await;
                                    success
                                }
                                Err(_) => false,
                            };
                            if !success {
                                page.fail(solstone_linux::import::STEP_ERROR);
                            }
                            if success {
                                page.clear_step();
                            }
                            button.set_sensitive(true);
                        });
                    });
                }
                let remove = gtk::Button::with_label("remove");
                remove.set_sensitive(row.can_remove);
                actions.append(&remove);
                let page = self.clone();
                let row = row.clone();
                remove.connect_clicked(move |_| page.confirm_remove(row.clone()));
                box_.append(&actions);
            }
            self.rows.append(&box_);
        }
    }

    fn confirm_remove(&self, row: ImportRow) {
        self.clear_step();
        let page = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok(client) = ControlClient::session().await else {
                page.fail(solstone_linux::import::STEP_ERROR);
                return;
            };
            let confirmation = client
                .import_begin_remove(&row.id)
                .await
                .ok()
                .and_then(|raw| serde_json::from_str::<Option<RemoveConfirmation>>(&raw).ok());
            match confirmation {
                Some(Some(confirmation)) => page.present_remove(Rc::new(client), confirmation),
                Some(None) => page.reload(&client).await,
                None => page.fail(solstone_linux::import::STEP_ERROR),
            }
        });
    }

    fn present_remove(&self, client: Rc<ControlClient>, confirmation: RemoveConfirmation) {
        let Some(window) = self.widget.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&window)
            .modal(true)
            .text(format!("remove {}?", confirmation.row.name))
            .secondary_text(warning(&confirmation.row))
            .build();
        dialog.add_button("cancel", gtk::ResponseType::Cancel);
        dialog.add_button("remove", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let error = label(solstone_linux::import::REMOVE_ERROR);
        error.add_css_class("sol-attn");
        error.set_visible(false);
        dialog.content_area().append(&error);
        let page = self.clone();
        dialog.connect_response(move |dialog, response| {
            error.set_visible(false);
            dialog.set_response_sensitive(gtk::ResponseType::Accept, false);
            let dialog = dialog.clone();
            let client = client.clone();
            let confirmation = confirmation.clone();
            let page = page.clone();
            let error = error.clone();
            glib::MainContext::default().spawn_local(async move {
                if response != gtk::ResponseType::Accept {
                    let _ = client
                        .import_cancel_remove(&confirmation.row.id, &confirmation.token)
                        .await;
                    dialog.close();
                    page.reload(&client).await;
                    return;
                }
                let result = client
                    .import_remove(
                        &confirmation.row.id,
                        &confirmation.token,
                        confirmation.row.may_have_saved,
                    )
                    .await;
                if result.ok().as_deref() == Some("removed") {
                    dialog.close();
                    page.reload(&client).await;
                    return;
                }
                error.set_visible(true);
                dialog.set_response_sensitive(gtk::ResponseType::Accept, true);
            });
        });
        dialog.present();
    }
}

fn warning(row: &ImportRow) -> &'static str {
    if row.missing {
        "the copy is already gone, so this only clears it from here."
    } else if row.may_have_saved {
        "this removes the copy on this device. your journal may still bring in the copy it already has."
    } else {
        "this removes the copy on this device, so it won't reach your journal."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_list_keeps_unsupported_and_nonlocal_inputs_visible_to_intake() {
        let paths = file_paths([
            gio::File::for_path("/tmp/note.txt"),
            gio::File::for_path("/tmp/movie.mp4"),
            gio::File::for_uri("https://example.invalid/file"),
        ]);
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[1], "/tmp/movie.mp4");
        assert_eq!(paths[2], "https://example.invalid/file");
        assert_eq!(
            drop_paths(&gio::File::for_path("/tmp/movie.mp4").to_value()).unwrap(),
            vec!["/tmp/movie.mp4"]
        );
        assert!(drop_paths(&42i32.to_value()).is_none());
    }
}
