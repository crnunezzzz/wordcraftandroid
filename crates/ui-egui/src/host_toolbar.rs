//! Optional Android assistant state, integrated into the native ribbon tabs.
#[derive(Default)]
pub struct HostToolbar {
    pub action: Option<&'static str>,
    pub assistant_open: bool,
    pub busy: bool,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    #[test]
    fn assistant_is_after_help_and_keeps_the_editor_tab() {
        let services = crate::Services { host_toolbar: Some(Default::default()), ..Default::default() };
        let app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Tablet")), services);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 900.0)).build_ui_state(
            |ui, app: &mut crate::WordApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(3);
        let help = h.get_by_label("Help").rect();
        let assistant = h.get_by_label("Asistente").rect();
        assert!(assistant.left() >= help.right());
        let tab = h.state().ui.tab.clone();
        h.get_by_label("Asistente").click();
        h.run_steps(2);
        assert_eq!(h.state().services.host_toolbar.as_ref().and_then(|h| h.action), Some("assistant"));
        assert_eq!(h.state().ui.tab, tab);
    }
}
