use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use studio_core::{Editor, Project};
use studio_import::{MediaStorageMode, import_media, migrate_legacy_projects};
use studio_jobs::{JobManager, JobStatus};
use studio_store::{ProjectLibrary, ProjectStore, ProjectSummary};
use uuid::Uuid;

const APPLICATION_ID: &str = "io.github.valtoo.PodcastStudio";

#[derive(Clone)]
struct AppState {
    root: PathBuf,
    window: adw::ApplicationWindow,
    project_list: gtk::ListBox,
    search: gtk::SearchEntry,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
    jobs: JobManager,
    active_jobs: Rc<RefCell<Vec<Uuid>>>,
}

fn main() {
    let app = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();

    app.connect_activate(build_main_window);
    app.run();
}

fn build_main_window(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Podcast Studio")
        .default_width(900)
        .default_height(700)
        .build();
    let heading = gtk::Label::builder()
        .label("Podcast Studio")
        .css_classes(["title-1"])
        .halign(gtk::Align::Start)
        .build();
    let description = gtk::Label::builder()
        .label("Projetos locais de edição de áudio e vídeo")
        .halign(gtk::Align::Start)
        .build();
    let new_project = gtk::Button::with_label("Importar mídia");
    new_project.add_css_class("suggested-action");
    let migrate_button = gtk::Button::with_label("Importar projetos JSON antigos…");
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Buscar projetos")
        .hexpand(true)
        .build();
    let project_list = gtk::ListBox::new();
    project_list.set_selection_mode(gtk::SelectionMode::None);
    project_list.add_css_class("boxed-list");
    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&project_list)
        .build();
    let status = gtk::Label::builder()
        .label("Carregando projetos…")
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    let progress = gtk::ProgressBar::builder()
        .show_text(true)
        .text("Sem tarefas em andamento")
        .build();
    let cancel = gtk::Button::with_label("Cancelar tarefas");
    cancel.set_sensitive(false);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.append(&new_project);
    toolbar.append(&migrate_button);
    toolbar.append(&search);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&heading);
    content.append(&description);
    content.append(&toolbar);
    content.append(&scrolled);
    content.append(&status);
    let job_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    job_controls.append(&progress);
    job_controls.append(&cancel);
    content.append(&job_controls);
    window.set_content(Some(&content));

    let root = match ProjectLibrary::default_root() {
        Ok(root) => root,
        Err(error) => {
            status.set_text(&format!(
                "Não foi possível determinar a pasta de dados: {error}"
            ));
            window.present();
            return;
        }
    };
    let state = AppState {
        root,
        window,
        project_list,
        search,
        status,
        progress,
        cancel,
        jobs: JobManager::new(),
        active_jobs: Rc::new(RefCell::new(Vec::new())),
    };

    let import_state = state.clone();
    new_project.connect_clicked(move |_| choose_media_file(import_state.clone()));

    let migration_state = state.clone();
    migrate_button.connect_clicked(move |_| choose_legacy_directory(migration_state.clone()));

    let search_state = state.clone();
    state
        .search
        .connect_search_changed(move |_| refresh_projects(&search_state));

    let cancel_state = state.clone();
    state.cancel.connect_clicked(move |_| {
        let active_ids = cancel_state.active_jobs.borrow().clone();
        for id in active_ids {
            cancel_state.jobs.cancel(id);
        }
        cancel_state.status.set_text("Cancelamento solicitado…");
    });

    refresh_projects(&state);
    state.window.present();
}

fn refresh_projects(state: &AppState) {
    while let Some(child) = state.project_list.first_child() {
        state.project_list.remove(&child);
    }

    let library = match ProjectLibrary::new(&state.root) {
        Ok(library) => library,
        Err(error) => {
            state
                .status
                .set_text(&format!("Não foi possível abrir a biblioteca: {error}"));
            return;
        }
    };
    let projects = match library.list() {
        Ok(projects) => projects,
        Err(error) => {
            state
                .status
                .set_text(&format!("Falha ao carregar projetos: {error}"));
            return;
        }
    };
    let query = state.search.text().trim().to_lowercase();
    let projects: Vec<_> = projects
        .into_iter()
        .filter(|project| {
            query.is_empty()
                || project.title.to_lowercase().contains(&query)
                || project.preview.to_lowercase().contains(&query)
        })
        .collect();
    if projects.is_empty() {
        let empty = gtk::Label::builder()
            .label(if query.is_empty() {
                "Nenhum projeto ainda. Importe um arquivo de áudio ou vídeo para começar."
            } else {
                "Nenhum projeto corresponde à busca."
            })
            .wrap(true)
            .margin_top(24)
            .margin_bottom(24)
            .build();
        state.project_list.append(&empty);
        return;
    }
    for project in projects {
        state
            .project_list
            .append(&build_project_row(state, project));
    }
}

fn build_project_row(state: &AppState, summary: ProjectSummary) -> gtk::ListBoxRow {
    let title = gtk::Label::builder()
        .label(&summary.title)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    let subtitle = if summary.preview.is_empty() {
        format!("{} · projeto local", format_duration(summary.duration_ms))
    } else {
        format!(
            "{} · {}",
            format_duration(summary.duration_ms),
            summary.preview
        )
    };
    let details = gtk::Label::builder()
        .label(subtitle)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    details.add_css_class("dim-label");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&details);

    let open = gtk::Button::with_label("Abrir");
    let open_state = state.clone();
    let open_id = summary.id;
    open.connect_clicked(move |_| {
        let result = ProjectLibrary::new(&open_state.root)
            .and_then(|library| library.open(open_id))
            .and_then(|store| store.load());
        match result {
            Ok(editor) => match editor.project.timeline() {
                Ok(timeline) => open_state.status.set_text(&format!(
                    "Projeto aberto: {} — {} bloco(s), {} ms.",
                    editor.project.title,
                    editor.project.blocks.len(),
                    timeline.duration_ms
                )),
                Err(error) => open_state
                    .status
                    .set_text(&format!("Projeto inválido: {error}")),
            },
            Err(error) => open_state
                .status
                .set_text(&format!("Falha ao abrir projeto: {error}")),
        }
    });

    let duplicate = gtk::Button::with_label("Duplicar");
    let duplicate_state = state.clone();
    let duplicate_id = summary.id;
    duplicate.connect_clicked(move |_| {
        let root = duplicate_state.root.clone();
        start_job(&duplicate_state, "Duplicando projeto", move |context| {
            context
                .check_cancelled()
                .map_err(|error| error.to_string())?;
            let library = ProjectLibrary::new(root).map_err(|error| error.to_string())?;
            let duplicate = library
                .duplicate_with_progress(duplicate_id, |fraction| {
                    context
                        .report_progress(fraction, "Copiando projeto")
                        .map_err(|_| studio_store::StoreError::Cancelled)
                })
                .map_err(|error| error.to_string())?;
            Ok(duplicate.id.to_string())
        });
    });

    let rename = gtk::Button::with_label("Renomear");
    let rename_state = state.clone();
    let rename_summary = summary.clone();
    rename.connect_clicked(move |_| prompt_rename(&rename_state, &rename_summary));

    let delete = gtk::Button::with_label("Excluir");
    delete.add_css_class("destructive-action");
    let delete_state = state.clone();
    delete.connect_clicked(move |_| confirm_delete(&delete_state, &summary));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.append(&open);
    actions.append(&duplicate);
    actions.append(&rename);
    actions.append(&delete);

    let row_content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row_content.set_margin_top(10);
    row_content.set_margin_bottom(10);
    row_content.set_margin_start(12);
    row_content.set_margin_end(12);
    row_content.append(&text);
    row_content.append(&actions);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&row_content));
    row.set_activatable(false);
    row
}

fn prompt_rename(state: &AppState, project: &ProjectSummary) {
    let dialog = adw::Window::builder()
        .transient_for(&state.window)
        .modal(true)
        .title("Renomear projeto")
        .default_width(420)
        .build();
    let entry = gtk::Entry::builder()
        .text(&project.title)
        .activates_default(true)
        .build();
    let cancel = gtk::Button::with_label("Cancelar");
    let confirm = gtk::Button::with_label("Salvar nome");
    confirm.add_css_class("suggested-action");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&confirm);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.append(&entry);
    content.append(&buttons);
    dialog.set_content(Some(&content));

    let close_dialog = dialog.clone();
    cancel.connect_clicked(move |_| close_dialog.close());
    let rename_dialog = dialog.clone();
    let rename_state = state.clone();
    let project_id = project.id;
    let rename_entry = entry.clone();
    confirm.connect_clicked(move |_| {
        let title = rename_entry.text().trim().to_owned();
        if title.is_empty() {
            rename_state
                .status
                .set_text("O nome do projeto não pode ficar vazio.");
            return;
        }
        rename_dialog.close();
        let root = rename_state.root.clone();
        start_job(&rename_state, "Renomeando projeto", move |context| {
            context
                .check_cancelled()
                .map_err(|error| error.to_string())?;
            let library = ProjectLibrary::new(root).map_err(|error| error.to_string())?;
            let summary = library
                .rename(project_id, &title)
                .map_err(|error| error.to_string())?;
            Ok(summary.title)
        });
    });
    dialog.present();
}

fn confirm_delete(state: &AppState, project: &ProjectSummary) {
    let dialog = adw::Window::builder()
        .transient_for(&state.window)
        .modal(true)
        .title("Excluir projeto?")
        .default_width(380)
        .build();
    let message = gtk::Label::builder()
        .label(format!(
            "O projeto “{}” e a mídia copiada para ele serão removidos. Esta ação não pode ser desfeita.",
            project.title
        ))
        .wrap(true)
        .build();
    let cancel = gtk::Button::with_label("Cancelar");
    let confirm = gtk::Button::with_label("Excluir projeto");
    confirm.add_css_class("destructive-action");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&confirm);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.append(&message);
    content.append(&buttons);
    dialog.set_content(Some(&content));

    let close_dialog = dialog.clone();
    cancel.connect_clicked(move |_| close_dialog.close());
    let delete_dialog = dialog.clone();
    let delete_state = state.clone();
    let project_id = project.id;
    confirm.connect_clicked(move |_| {
        delete_dialog.close();
        let root = delete_state.root.clone();
        start_job(&delete_state, "Excluindo projeto", move |context| {
            context
                .check_cancelled()
                .map_err(|error| error.to_string())?;
            ProjectLibrary::new(root)
                .and_then(|library| library.delete(project_id))
                .map_err(|error| error.to_string())?;
            Ok(project_id.to_string())
        });
    });
    dialog.present();
}

fn choose_media_file(state: AppState) {
    let dialog = gtk::FileDialog::builder()
        .title("Importar áudio ou vídeo")
        .build();
    let parent = state.window.clone();
    dialog.open(
        Some(&parent),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    state
                        .status
                        .set_text("O arquivo selecionado não tem caminho local.");
                    return;
                };
                choose_media_storage(&state, path);
            }
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(error) => state
                .status
                .set_text(&format!("Falha ao selecionar arquivo: {error}")),
        },
    );
}

fn choose_media_storage(state: &AppState, source: PathBuf) {
    let dialog = adw::Window::builder()
        .transient_for(&state.window)
        .modal(true)
        .title("Armazenamento da mídia")
        .default_width(460)
        .build();
    let message = gtk::Label::builder()
        .label("Copiar mantém o projeto autocontido. Referenciar evita duplicar o arquivo, mas ele precisa permanecer no mesmo caminho.")
        .wrap(true)
        .build();
    let copy = gtk::Button::with_label("Copiar para o projeto (recomendado)");
    copy.add_css_class("suggested-action");
    let reference = gtk::Button::with_label("Referenciar arquivo original");
    let cancel = gtk::Button::with_label("Cancelar");
    let buttons = gtk::Box::new(gtk::Orientation::Vertical, 8);
    buttons.append(&copy);
    buttons.append(&reference);
    buttons.append(&cancel);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.append(&message);
    content.append(&buttons);
    dialog.set_content(Some(&content));

    let cancel_dialog = dialog.clone();
    cancel.connect_clicked(move |_| cancel_dialog.close());
    let copy_dialog = dialog.clone();
    let copy_state = state.clone();
    let copy_source = source.clone();
    copy.connect_clicked(move |_| {
        copy_dialog.close();
        start_media_import(
            &copy_state,
            copy_source.clone(),
            MediaStorageMode::CopyIntoProject,
        );
    });
    let reference_dialog = dialog.clone();
    let reference_state = state.clone();
    reference.connect_clicked(move |_| {
        reference_dialog.close();
        start_media_import(
            &reference_state,
            source.clone(),
            MediaStorageMode::ReferenceOriginal,
        );
    });
    dialog.present();
}

fn start_media_import(state: &AppState, source: PathBuf, storage_mode: MediaStorageMode) {
    let root = state.root.clone();
    start_job(state, "Importando mídia", move |context| {
        let project_id = Uuid::new_v4();
        let project_dir = root.join(project_id.to_string());
        let result = (|| {
            context
                .check_cancelled()
                .map_err(|error| error.to_string())?;
            let media = import_media(&source, &project_dir, storage_mode, &context)
                .map_err(|error| error.to_string())?;
            context
                .report_progress(0.96, "Salvando projeto")
                .map_err(|error| error.to_string())?;
            let title = source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .unwrap_or("Novo projeto");
            let mut project = Project::new(title);
            project.media.push(media);
            let editor = Editor::new(project).map_err(|error| error.to_string())?;
            ProjectStore::create(&project_dir, &editor).map_err(|error| error.to_string())?;
            Ok(project_id.to_string())
        })();
        if result.is_err() && project_dir.exists() {
            std::fs::remove_dir_all(&project_dir)
                .map_err(|error| format!("Falha ao limpar importação incompleta: {error}"))?;
        }
        result
    });
}

fn choose_legacy_directory(state: AppState) {
    let dialog = gtk::FileDialog::builder()
        .title("Selecionar a pasta data do app antigo")
        .build();
    let parent = state.window.clone();
    dialog.select_folder(
        Some(&parent),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(folder) => {
                let Some(legacy_root) = folder.path() else {
                    state
                        .status
                        .set_text("A pasta selecionada não tem caminho local.");
                    return;
                };
                let projects_root = state.root.clone();
                start_job(&state, "Importando projetos antigos", move |context| {
                    let imported = migrate_legacy_projects(&legacy_root, &projects_root, &context)
                        .map_err(|error| error.to_string())?;
                    Ok(format!("{imported} projeto(s) importado(s)"))
                });
            }
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(error) => state
                .status
                .set_text(&format!("Falha ao selecionar pasta: {error}")),
        },
    );
}

fn start_job(
    state: &AppState,
    label: &str,
    work: impl FnOnce(studio_jobs::JobContext) -> Result<String, String> + Send + 'static,
) {
    let id = state.jobs.start(label, work);
    state.active_jobs.borrow_mut().push(id);
    state.cancel.set_sensitive(true);
    state.progress.set_fraction(0.0);
    state.progress.set_text(Some(label));

    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let active = state.active_jobs.borrow().clone();
        let mut pending = Vec::new();
        let mut completed_any = false;
        for id in active {
            let Some(snapshot) = state.jobs.snapshot(id) else {
                continue;
            };
            state.progress.set_fraction(snapshot.progress);
            state.progress.set_text(Some(&snapshot.message));
            match snapshot.status {
                JobStatus::Queued | JobStatus::Running => {
                    pending.push(id);
                    state.status.set_text(&format!(
                        "{} — {:.0}%",
                        snapshot.label,
                        snapshot.progress * 100.0
                    ));
                }
                JobStatus::Completed => {
                    state.status.set_text(&format!(
                        "{}: {}",
                        snapshot.label,
                        snapshot.result.unwrap_or_else(|| "concluído".into())
                    ));
                    completed_any = true;
                }
                JobStatus::Failed => {
                    state.status.set_text(&format!(
                        "{}: {}",
                        snapshot.label,
                        snapshot.error.unwrap_or_else(|| "erro sem detalhes".into())
                    ));
                    completed_any = true;
                }
                JobStatus::Cancelled => {
                    state
                        .status
                        .set_text(&format!("{}: operação cancelada.", snapshot.label));
                    completed_any = true;
                }
            }
        }
        *state.active_jobs.borrow_mut() = pending;
        if state.active_jobs.borrow().is_empty() {
            state.cancel.set_sensitive(false);
            if completed_any {
                refresh_projects(&state);
            }
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn format_duration(duration_ms: i64) -> String {
    let total_seconds = duration_ms.max(0) / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
