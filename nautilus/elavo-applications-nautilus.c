/* The Applications folder in Files.
 *
 * - Opening an entry (double-click, Enter) starts the application instead of
 *   showing the directory. Nautilus 50 opens folders through internal calls,
 *   so the module watches each window slot's location: when it goes from a
 *   folder into an .eapp entry in that folder, it starts the entry and goes
 *   back. Opening an entry in a new tab or window shows its contents.
 * - Move to Trash and Delete refuse an entry whose application is running.
 *   The view's actions are replaced by wrappers that check first and then
 *   call Nautilus's own action. Emission hooks cannot stop a signal.
 * - A drag that starts on an entry never moves or copies the install.
 *   Wayland has no link action, so the drag offers a copy. Dropped in a
 *   Files window, on a folder, a sidebar place, or the path bar, the entry
 *   is swapped for a shortcut file and Nautilus copies that; inside an
 *   Applications folder the drop does nothing. The Dock pins it and the
 *   desktop makes its own shortcut.
 * - Item labels leave out the .eapp suffix.
 * - A drop into a shared folder this account cannot write asks for an
 *   administrator's password (elavo-applications-ctl install --shared).
 *
 * Nautilus loads this from <libdir>/nautilus/extensions-4. For testing on a
 * read-only /usr it also starts when preloaded:
 *   ELAVO_APPLICATIONS_PRELOAD=1 LD_PRELOAD=$PWD/libelavo-applications.so nautilus
 */

#include <dlfcn.h>
#include <gio/gdesktopappinfo.h>
#include <glib/gstdio.h>
#include <gtk/gtk.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define SUFFIX ".eapp"
#define WRAPPED "elavo-applications-wrapped"
#define WATCHED "elavo-applications-watched"
#define PREVIOUS "elavo-applications-previous"
#define BLOCKED "elavo-applications-blocked"
#define PASSING "elavo-applications-passing"

typedef GFile *(*FileInfoGetLocation)(gpointer file);
typedef gpointer (*AdwAlertDialogNew)(const char *heading, const char *body);
typedef void (*AdwAlertDialogAddResponse)(gpointer dialog, const char *id, const char *label);
typedef void (*AdwDialogPresent)(gpointer dialog, GtkWidget *parent);

static gboolean started;
static FileInfoGetLocation file_info_get_location;
static GType slot_type;
static GType files_view_type;
static GType view_cell_type;
static GType sidebar_type;
static GType path_bar_type;

/* ---- Paths ---- */

static char *
real(const char *path)
{
    char buffer[PATH_MAX];

    return path && realpath(path, buffer) ? g_strdup(buffer) : NULL;
}

static const char *
ctl(void)
{
    const char *path = g_getenv("ELAVO_APPLICATIONS_CTL");

    return path && *path ? path : "elavo-applications-ctl";
}

static char *
shared_dir(void)
{
    const char *path = g_getenv("ELAVO_SHARED_APPLICATIONS");

    return real(path && *path ? path : "/Applications");
}

static gboolean
is_bundle(const char *path)
{
    char *entry;
    gboolean result;

    if (!path || !g_str_has_suffix(path, SUFFIX))
        return FALSE;
    entry = g_build_filename(path, "entry.json", NULL);
    result = g_file_test(entry, G_FILE_TEST_IS_REGULAR);
    g_free(entry);
    return result;
}

static char *
file_path(GObject *file)
{
    GFile *location;
    char *path;

    if (!file || !file_info_get_location)
        return NULL;
    location = file_info_get_location(file);
    if (!location)
        return NULL;
    path = g_file_get_path(location);
    g_object_unref(location);
    return path;
}

/* The NautilusFile behind a list item: a NautilusViewItem, or a
 * GtkTreeListRow around one in the list view. */
static char *
item_path(GObject *item)
{
    GObject *file = NULL;
    char *path;

    if (GTK_IS_TREE_LIST_ROW(item)) {
        GObject *inner = gtk_tree_list_row_get_item(GTK_TREE_LIST_ROW(item));
        path = item_path(inner);
        g_clear_object(&inner);
        return path;
    }
    if (!item || !g_object_class_find_property(G_OBJECT_GET_CLASS(item), "file"))
        return NULL;
    g_object_get(item, "file", &file, NULL);
    path = file_path(file);
    g_clear_object(&file);
    return path;
}

static char *
cell_path(GtkWidget *cell)
{
    GObject *item = NULL;
    char *path;

    if (!cell || !g_object_class_find_property(G_OBJECT_GET_CLASS(cell), "item"))
        return NULL;
    g_object_get(cell, "item", &item, NULL);
    path = item_path(item);
    g_clear_object(&item);
    return path;
}

static char *
slot_path(GtkWidget *slot)
{
    GFile *location = NULL;
    char *path;

    if (!slot || !g_object_class_find_property(G_OBJECT_GET_CLASS(slot), "location"))
        return NULL;
    g_object_get(slot, "location", &location, NULL);
    path = location ? g_file_get_path(location) : NULL;
    g_clear_object(&location);
    return path;
}

/* ---- Widgets ---- */

static GtkWidget *
find_descendant(GtkWidget *widget, GType type)
{
    GtkWidget *child;

    if (!widget)
        return NULL;
    if (g_type_check_instance_is_a((GTypeInstance *)widget, type))
        return widget;
    for (child = gtk_widget_get_first_child(widget); child; child = gtk_widget_get_next_sibling(child)) {
        GtkWidget *found = find_descendant(child, type);
        if (found)
            return found;
    }
    return NULL;
}

static GtkWidget *
ancestor(GtkWidget *widget, GType type)
{
    return type && widget ? gtk_widget_get_ancestor(widget, type) : NULL;
}

/* The paths of the selected items in a files view. */
static GPtrArray *
selected_paths(GtkWidget *view)
{
    GPtrArray *paths = g_ptr_array_new_with_free_func(g_free);
    GtkWidget *list = find_descendant(view, GTK_TYPE_GRID_VIEW);
    GtkSelectionModel *model = NULL;
    GtkBitset *selection;
    GtkBitsetIter iter;
    guint position;

    if (list)
        model = gtk_grid_view_get_model(GTK_GRID_VIEW(list));
    else if ((list = find_descendant(view, GTK_TYPE_COLUMN_VIEW)))
        model = gtk_column_view_get_model(GTK_COLUMN_VIEW(list));
    if (!model)
        return paths;
    selection = gtk_selection_model_get_selection(model);
    if (gtk_bitset_iter_init_first(&iter, selection, &position)) {
        do {
            GObject *item = g_list_model_get_item(G_LIST_MODEL(model), position);
            char *path = item_path(item);
            if (path)
                g_ptr_array_add(paths, path);
            g_clear_object(&item);
        } while (gtk_bitset_iter_next(&iter, &position));
    }
    gtk_bitset_unref(selection);
    return paths;
}

static void
tell(GtkWidget *parent, const char *heading, const char *body)
{
    static AdwAlertDialogNew dialog_new;
    static AdwAlertDialogAddResponse add_response;
    static AdwDialogPresent present;
    gpointer dialog;

    if (!dialog_new) {
        dialog_new = (AdwAlertDialogNew)dlsym(RTLD_DEFAULT, "adw_alert_dialog_new");
        add_response = (AdwAlertDialogAddResponse)dlsym(RTLD_DEFAULT, "adw_alert_dialog_add_response");
        present = (AdwDialogPresent)dlsym(RTLD_DEFAULT, "adw_dialog_present");
    }
    /* An AdwDialog is drawn inside the window, so no toplevel is made and
     * destroyed for it. */
    if (!dialog_new || !add_response || !present) {
        g_warning("elavo applications: %s %s", heading, body);
        return;
    }
    dialog = dialog_new(heading, body);
    add_response(dialog, "ok", "OK");
    present(dialog, parent);
}

static char *
entry_name(const char *path)
{
    char *base = g_path_get_basename(path);

    if (g_str_has_suffix(base, SUFFIX))
        base[strlen(base) - strlen(SUFFIX)] = '\0';
    return base;
}

static gboolean
running(const char *bundle)
{
    const char *argv[] = { ctl(), "running", bundle, NULL };
    int status = 0;

    if (!g_spawn_sync(NULL, (char **)argv, NULL, G_SPAWN_SEARCH_PATH | G_SPAWN_STDOUT_TO_DEV_NULL | G_SPAWN_STDERR_TO_DEV_NULL,
                      NULL, NULL, NULL, NULL, &status, NULL))
        return FALSE;
    return g_spawn_check_wait_status(status, NULL);
}

/* ---- Opening an entry ---- */

typedef struct {
    GtkWidget *slot;
    char *bundle;
} Opened;

static gboolean
launch_and_return(gpointer data)
{
    Opened *opened = data;
    char *desktop = g_build_filename(opened->bundle, "launch.desktop", NULL);
    GDesktopAppInfo *info = g_desktop_app_info_new_from_filename(desktop);
    GError *error = NULL;

    gtk_widget_activate_action(opened->slot, "slot.back", NULL);
    if (info) {
        GdkAppLaunchContext *context = gdk_display_get_app_launch_context(gtk_widget_get_display(opened->slot));
        if (!g_app_info_launch(G_APP_INFO(info), NULL, G_APP_LAUNCH_CONTEXT(context), &error)) {
            char *name = entry_name(opened->bundle);
            char *heading = g_strdup_printf("“%s” could not be opened", name);
            tell(opened->slot, heading, error->message);
            g_free(heading);
            g_free(name);
            g_clear_error(&error);
        }
        g_object_unref(context);
        g_object_unref(info);
    }
    g_free(desktop);
    g_object_unref(opened->slot);
    g_free(opened->bundle);
    g_free(opened);
    return G_SOURCE_REMOVE;
}

static void
on_location(GObject *object, GParamSpec *pspec, gpointer data)
{
    GtkWidget *slot = GTK_WIDGET(object);
    char *path = slot_path(slot);
    char *resolved = real(path);
    const char *previous = g_object_get_data(object, PREVIOUS);
    char *parent = path ? g_path_get_dirname(path) : NULL;

    (void)pspec;
    (void)data;
    if (resolved && is_bundle(resolved) && previous && g_strcmp0(previous, parent) == 0) {
        Opened *opened = g_new0(Opened, 1);
        opened->slot = g_object_ref(slot);
        opened->bundle = g_strdup(resolved);
        g_idle_add(launch_and_return, opened);
    }
    g_object_set_data_full(object, PREVIOUS, g_strdup(path), g_free);
    g_free(parent);
    g_free(resolved);
    g_free(path);
}

static void
watch_slot(GtkWidget *slot)
{
    if (g_object_get_data(G_OBJECT(slot), WATCHED))
        return;
    g_object_set_data(G_OBJECT(slot), WATCHED, GINT_TO_POINTER(1));
    if (!g_object_class_find_property(G_OBJECT_GET_CLASS(slot), "location")) {
        g_warning("elavo applications: NautilusWindowSlot has no location property");
        return;
    }
    g_object_set_data_full(G_OBJECT(slot), PREVIOUS, slot_path(slot), g_free);
    g_signal_connect(slot, "notify::location", G_CALLBACK(on_location), NULL);
}

/* ---- Move to Trash ---- */

typedef struct {
    GAction *original;
    GtkWidget *view;
} Wrapped;

static void
on_wrapped_activate(GSimpleAction *action, GVariant *parameter, gpointer data)
{
    Wrapped *wrapped = data;
    GPtrArray *paths = selected_paths(wrapped->view);
    guint i;

    (void)action;
    for (i = 0; i < paths->len; i++) {
        const char *path = g_ptr_array_index(paths, i);
        char *resolved = real(path);
        gboolean busy = resolved && is_bundle(resolved) && running(resolved);
        g_free(resolved);
        if (busy) {
            char *name = entry_name(path);
            g_message("elavo applications: %s is running; not moving it to the Trash", path);
            char *heading = g_strdup_printf("“%s” is open", name);
            char *body = g_strdup_printf("Quit %s, then move it to the Trash.", name);
            tell(wrapped->view, heading, body);
            g_free(body);
            g_free(heading);
            g_free(name);
            g_ptr_array_unref(paths);
            return;
        }
    }
    g_ptr_array_unref(paths);
    g_action_activate(wrapped->original, parameter);
}

static void
wrapped_free(gpointer data, GClosure *closure)
{
    Wrapped *wrapped = data;

    (void)closure;
    g_object_unref(wrapped->original);
    g_free(wrapped);
}

typedef struct {
    guintptr start;
    guintptr end;
} MapRange;

static GArray *
load_maps(void)
{
    GArray *maps = g_array_new(FALSE, FALSE, sizeof(MapRange));
    FILE *file = fopen("/proc/self/maps", "r");
    char line[512];

    if (!file)
        return maps;
    while (fgets(line, sizeof line, file)) {
        unsigned long start = 0, end = 0;
        char perms[5] = "";
        if (sscanf(line, "%lx-%lx %4s", &start, &end, perms) == 3 && perms[0] == 'r') {
            MapRange range = { start, end };
            g_array_append_val(maps, range);
        }
    }
    fclose(file);
    return maps;
}

static gboolean
readable(GArray *maps, const void *pointer, gsize length)
{
    guintptr start = (guintptr)pointer;
    guintptr end = start + length;
    guint i;

    if (!pointer || end < start)
        return FALSE;
    for (i = 0; i < maps->len; i++) {
        MapRange *range = &g_array_index(maps, MapRange, i);
        if (start >= range->start && end <= range->end)
            return TRUE;
    }
    return FALSE;
}

/* The GActionGroup a NautilusFilesView keeps for its view.* actions. GTK 4
 * has no getter for an inserted group, so the field is found by scanning
 * the object for an action map that holds `probe`, the way Folio's module
 * does. Every pointer is checked against /proc/self/maps before it is read,
 * and the offset is remembered once found. */
static GActionMap *
view_group(GObject *view, const char *probe)
{
    static gsize offset;
    GArray *maps;
    gsize at;
    GActionMap *found = NULL;

    if (offset) {
        gpointer candidate = *(gpointer *)((char *)view + offset);
        if (G_IS_ACTION_MAP(candidate) && g_action_map_lookup_action(candidate, probe))
            return candidate;
    }
    maps = load_maps();
    for (at = sizeof(gpointer); at < 4096; at += sizeof(gpointer)) {
        gpointer candidate;
        GTypeClass *klass;

        if (!readable(maps, (char *)view + at, sizeof(gpointer)))
            break;
        candidate = *(gpointer *)((char *)view + at);
        if (!candidate || ((guintptr)candidate & (sizeof(gpointer) - 1)) != 0)
            continue;
        if (!readable(maps, candidate, sizeof(gpointer)))
            continue;
        klass = *(GTypeClass **)candidate;
        if (!readable(maps, klass, sizeof(GTypeClass)))
            continue;
        if (klass->g_type == 0 || g_type_name(klass->g_type) == NULL)
            continue;
        if (!G_IS_ACTION_MAP(candidate) || !g_action_map_lookup_action(candidate, probe))
            continue;
        offset = at;
        found = candidate;
        break;
    }
    g_array_unref(maps);
    return found;
}

static void
wrap_action(GActionMap *map, const char *name, GtkWidget *view)
{
    GAction *original = g_action_map_lookup_action(map, name);
    GSimpleAction *wrapper;
    Wrapped *wrapped;

    if (!original || g_object_get_data(G_OBJECT(original), WRAPPED))
        return;
    wrapper = g_simple_action_new(name, g_action_get_parameter_type(original));
    g_simple_action_set_enabled(wrapper, g_action_get_enabled(original));
    g_object_set_data(G_OBJECT(wrapper), WRAPPED, GINT_TO_POINTER(1));
    wrapped = g_new0(Wrapped, 1);
    wrapped->original = g_object_ref(original);
    wrapped->view = view;
    g_signal_connect_data(wrapper, "activate", G_CALLBACK(on_wrapped_activate), wrapped, wrapped_free, 0);
    /* Nautilus looks its actions up by name when it updates them, so the
     * wrapper gets the enabled state from then on. */
    g_action_map_add_action(map, G_ACTION(wrapper));
    g_object_unref(wrapper);
}

static void
watch_view(GtkWidget *view)
{
    GActionMap *map;

    if (g_object_get_data(G_OBJECT(view), WATCHED))
        return;
    g_object_set_data(G_OBJECT(view), WATCHED, GINT_TO_POINTER(1));
    map = view_group(G_OBJECT(view), "move-to-trash");
    if (!map) {
        g_warning("elavo applications: no view actions found");
        return;
    }
    wrap_action(map, "move-to-trash", view);
    wrap_action(map, "delete-permanently", view);
    wrap_action(map, "permanent-delete", view);
}

/* ---- Labels ---- */

static void
trim_label(GObject *object, GParamSpec *pspec, gpointer data)
{
    const char *property = data;
    char *text = NULL;

    (void)pspec;
    g_object_get(object, property, &text, NULL);
    if (text && g_str_has_suffix(text, SUFFIX) && strlen(text) > strlen(SUFFIX)) {
        text[strlen(text) - strlen(SUFFIX)] = '\0';
        g_object_set(object, property, text, NULL);
    }
    g_free(text);
}

static void
watch_label(GtkWidget *label)
{
    const char *property = GTK_IS_LABEL(label) ? "label" : "text";
    char *signal;

    if (g_object_get_data(G_OBJECT(label), WATCHED))
        return;
    g_object_set_data(G_OBJECT(label), WATCHED, GINT_TO_POINTER(1));
    signal = g_strconcat("notify::", property, NULL);
    g_signal_connect(label, signal, G_CALLBACK(trim_label), (gpointer)property);
    g_free(signal);
    trim_label(G_OBJECT(label), NULL, (gpointer)property);
}

/* ---- Drops into a shared folder this account cannot write ---- */

static gboolean
installable(const char *path)
{
    return g_str_has_suffix(path, ".AppImage") || g_str_has_suffix(path, ".appimage") ||
           g_str_has_suffix(path, ".flatpakref") || g_str_has_suffix(path, ".flatpak");
}

/* The folder a drop lands in, where that is known: the folder item it was
 * dropped on, or the folder the view shows. NULL for the sidebar and the
 * path bar. */
static char *
drop_folder(GtkWidget *widget)
{
    if (view_cell_type && g_type_check_instance_is_a((GTypeInstance *)widget, view_cell_type)) {
        char *path = cell_path(widget);
        if (path && g_file_test(path, G_FILE_TEST_IS_DIR))
            return path;
        g_free(path);
    }
    return slot_path(ancestor(widget, slot_type));
}

/* Inside ~/Applications or the shared folder, or one of them. */
static gboolean
in_applications(const char *resolved)
{
    char *home = g_build_filename(g_get_home_dir(), "Applications", NULL);
    char *dirs[] = { real(home), shared_dir() };
    gboolean inside = FALSE;
    guint i;

    for (i = 0; i < G_N_ELEMENTS(dirs); i++) {
        gsize n = dirs[i] ? strlen(dirs[i]) : 0;
        if (n && strncmp(resolved, dirs[i], n) == 0 && (resolved[n] == '\0' || resolved[n] == '/'))
            inside = TRUE;
        g_free(dirs[i]);
    }
    g_free(home);
    return inside;
}

/* A shortcut file for `bundle`, written in a folder of its own under the
 * runtime directory for Nautilus to copy. Folders from earlier drops are
 * removed once they are an hour old. */
static GFile *
shortcut_for(const char *bundle)
{
    char *root = g_build_filename(g_get_user_runtime_dir(), "elavo-applications", "shortcuts", NULL);
    char *dir;
    char *out = NULL;
    GFile *result = NULL;
    GDir *old;
    const char *name;

    g_mkdir_with_parents(root, 0700);
    if ((old = g_dir_open(root, 0, NULL))) {
        while ((name = g_dir_read_name(old))) {
            char *path = g_build_filename(root, name, NULL);
            struct stat st;
            if (stat(path, &st) == 0 && time(NULL) - st.st_mtime > 3600) {
                GDir *inner = g_dir_open(path, 0, NULL);
                const char *file;
                while (inner && (file = g_dir_read_name(inner))) {
                    char *child = g_build_filename(path, file, NULL);
                    g_unlink(child);
                    g_free(child);
                }
                if (inner)
                    g_dir_close(inner);
                g_rmdir(path);
            }
            g_free(path);
        }
        g_dir_close(old);
    }
    dir = g_build_filename(root, "XXXXXX", NULL);
    if (g_mkdtemp(dir)) {
        const char *argv[] = { ctl(), "shortcut", bundle, dir, NULL };
        int status = 0;
        if (g_spawn_sync(NULL, (char **)argv, NULL, G_SPAWN_SEARCH_PATH | G_SPAWN_STDERR_TO_DEV_NULL, NULL, NULL,
                         &out, NULL, &status, NULL) &&
            g_spawn_check_wait_status(status, NULL) && out) {
            g_strstrip(out);
            if (*out)
                result = g_file_new_for_path(out);
        }
    }
    g_free(out);
    g_free(dir);
    g_free(root);
    return result;
}

/* Decides a drop before Nautilus does. TRUE ends it. Otherwise, when
 * `replacement` was filled in, Nautilus gets that file list instead. */
static gboolean
on_drop(GtkDropTarget *target, const GValue *value, GValue *replacement)
{
    GtkWidget *widget = gtk_event_controller_get_widget(GTK_EVENT_CONTROLLER(target));
    char *folder = drop_folder(widget);
    char *resolved = real(folder);
    char *shared = shared_dir();
    gboolean handled = FALSE;
    gboolean bundles = FALSE;
    GSList *files, *l, *swapped = NULL;

    if (!G_VALUE_HOLDS(value, GDK_TYPE_FILE_LIST))
        goto out;
    files = gdk_file_list_get_files(g_value_get_boxed(value));

    /* An entry dragged out of Applications becomes a shortcut wherever it
     * lands, and nothing at all inside an Applications folder. The drag
     * offers a copy, the one action Wayland has that suits, so Nautilus is
     * handed the shortcut to copy instead of the install. */
    for (l = files; l; l = l->next) {
        char *path = g_file_get_path(l->data);
        char *bundle = real(path);
        if (bundle && is_bundle(bundle)) {
            GFile *shortcut = NULL;
            bundles = TRUE;
            if (!(resolved && in_applications(resolved)))
                shortcut = shortcut_for(bundle);
            if (shortcut)
                swapped = g_slist_prepend(swapped, shortcut);
        } else {
            swapped = g_slist_prepend(swapped, g_object_ref(l->data));
        }
        g_free(bundle);
        g_free(path);
    }
    if (bundles) {
        swapped = g_slist_reverse(swapped);
        if (swapped) {
            g_value_init(replacement, GDK_TYPE_FILE_LIST);
            g_value_take_boxed(replacement, gdk_file_list_new_from_list(swapped));
        } else {
            handled = TRUE;
        }
        g_slist_free_full(swapped, g_object_unref);
        g_slist_free(files);
        goto out;
    }
    g_slist_free_full(swapped, g_object_unref);

    /* Returning TRUE ends the drop before Nautilus tries the copy itself and
     * reports that the folder is read-only. */
    if (resolved && g_strcmp0(resolved, shared) == 0 && access(resolved, W_OK) != 0) {
        for (l = files; l; l = l->next) {
            char *path = g_file_get_path(l->data);
            if (path && installable(path)) {
                const char *argv[] = { ctl(), "install", path, "--shared", NULL };
                g_spawn_async(NULL, (char **)argv, NULL, G_SPAWN_SEARCH_PATH, NULL, NULL, NULL, NULL);
                handled = TRUE;
            }
            g_free(path);
        }
    }
    g_slist_free(files);
out:
    g_free(shared);
    g_free(resolved);
    g_free(folder);
    return handled;
}

/* "drop" stops at the first handler that returns TRUE, and handlers run in
 * the order they were connected. Nautilus's handlers are blocked so this
 * one decides first; anything it does not take is handed back to them. */
static gboolean
on_drop_first(GtkDropTarget *target, const GValue *value, double x, double y, gpointer data)
{
    GArray *blocked = g_object_get_data(G_OBJECT(target), BLOCKED);
    GValue replacement = G_VALUE_INIT;
    gboolean result = FALSE;
    guint i;

    (void)data;
    if (g_object_get_data(G_OBJECT(target), PASSING))
        return FALSE;
    if (on_drop(target, value, &replacement))
        return TRUE;
    if (blocked && blocked->len > 0) {
        for (i = 0; i < blocked->len; i++)
            g_signal_handler_unblock(target, g_array_index(blocked, gulong, i));
        g_object_set_data(G_OBJECT(target), PASSING, GINT_TO_POINTER(1));
        g_signal_emit_by_name(target, "drop", G_IS_VALUE(&replacement) ? &replacement : value, x, y, &result);
        g_object_set_data(G_OBJECT(target), PASSING, NULL);
        for (i = 0; i < blocked->len; i++)
            g_signal_handler_block(target, g_array_index(blocked, gulong, i));
    }
    if (G_IS_VALUE(&replacement))
        g_value_unset(&replacement);
    return result;
}

static void
watch_drop_targets(GtkWidget *widget)
{
    GListModel *controllers;
    guint i, n;

    controllers = gtk_widget_observe_controllers(widget);
    n = g_list_model_get_n_items(controllers);
    for (i = 0; i < n; i++) {
        GObject *controller = g_list_model_get_item(controllers, i);
        if (GTK_IS_DROP_TARGET(controller) && !g_object_get_data(controller, WATCHED)) {
            GArray *blocked = g_array_new(FALSE, FALSE, sizeof(gulong));
            guint signal = g_signal_lookup("drop", GTK_TYPE_DROP_TARGET);
            gulong id;

            g_object_set_data(controller, WATCHED, GINT_TO_POINTER(1));
            while ((id = g_signal_handler_find(controller, G_SIGNAL_MATCH_ID | G_SIGNAL_MATCH_UNBLOCKED, signal, 0,
                                               NULL, NULL, NULL))) {
                g_signal_handler_block(controller, id);
                g_array_append_val(blocked, id);
            }
            g_object_set_data_full(controller, BLOCKED, blocked, (GDestroyNotify)g_array_unref);
            g_signal_connect(controller, "drop", G_CALLBACK(on_drop_first), NULL);
        }
        g_object_unref(controller);
    }
    g_object_unref(controllers);
}

/* ---- Drags ---- */

static GdkDragAction saved_actions;
static GtkDragSource *dragging;

static gboolean
on_prepare_hook(GSignalInvocationHint *hint, guint n, const GValue *values, gpointer data)
{
    GtkDragSource *source = g_value_get_object(&values[0]);
    GtkWidget *cell = gtk_event_controller_get_widget(GTK_EVENT_CONTROLLER(source));
    char *path;
    char *resolved;

    (void)hint;
    (void)n;
    (void)data;
    if (!view_cell_type || !g_type_check_instance_is_a((GTypeInstance *)cell, view_cell_type))
        return TRUE;
    path = cell_path(cell);
    resolved = real(path);
    if (resolved && is_bundle(resolved)) {
        dragging = g_object_ref(source);
        saved_actions = gtk_drag_source_get_actions(source);
        gtk_drag_source_set_actions(source, GDK_ACTION_COPY);
    }
    g_free(resolved);
    g_free(path);
    return TRUE;
}

static gboolean
on_drag_end_hook(GSignalInvocationHint *hint, guint n, const GValue *values, gpointer data)
{
    GtkDragSource *source = g_value_get_object(&values[0]);

    (void)hint;
    (void)n;
    (void)data;
    if (dragging && source == dragging) {
        gtk_drag_source_set_actions(source, saved_actions);
        g_clear_object(&dragging);
    }
    return TRUE;
}

/* ---- Setup ---- */

static gboolean
on_map_hook(GSignalInvocationHint *hint, guint n, const GValue *values, gpointer data)
{
    GtkWidget *widget = g_value_get_object(&values[0]);

    (void)hint;
    (void)n;
    (void)data;
    if (!slot_type)
        slot_type = g_type_from_name("NautilusWindowSlot");
    if (!files_view_type)
        files_view_type = g_type_from_name("NautilusFilesView");
    if (!view_cell_type)
        view_cell_type = g_type_from_name("NautilusViewCell");
    if (!sidebar_type)
        sidebar_type = g_type_from_name("NautilusSidebar");
    if (!path_bar_type)
        path_bar_type = g_type_from_name("NautilusPathBar");

    if (slot_type && g_type_check_instance_is_a((GTypeInstance *)widget, slot_type))
        watch_slot(widget);
    else if (files_view_type && g_type_check_instance_is_a((GTypeInstance *)widget, files_view_type))
        watch_view(widget);
    else if ((GTK_IS_LABEL(widget) || GTK_IS_INSCRIPTION(widget)) && ancestor(widget, view_cell_type))
        watch_label(widget);
    else if ((GTK_IS_GRID_VIEW(widget) || GTK_IS_COLUMN_VIEW(widget)) && ancestor(widget, files_view_type))
        watch_drop_targets(widget);
    else if ((view_cell_type && g_type_check_instance_is_a((GTypeInstance *)widget, view_cell_type)) ||
             ancestor(widget, sidebar_type) || ancestor(widget, path_bar_type))
        watch_drop_targets(widget);
    return TRUE;
}

static void
start(void)
{
    if (started)
        return;
    started = TRUE;
    file_info_get_location = (FileInfoGetLocation)dlsym(RTLD_DEFAULT, "nautilus_file_info_get_location");
    /* Signals exist once their class is initialized. */
    g_type_class_ref(GTK_TYPE_WIDGET);
    g_type_class_ref(GTK_TYPE_DRAG_SOURCE);
    g_signal_add_emission_hook(g_signal_lookup("map", GTK_TYPE_WIDGET), 0, on_map_hook, NULL, NULL);
    g_signal_add_emission_hook(g_signal_lookup("prepare", GTK_TYPE_DRAG_SOURCE), 0, on_prepare_hook, NULL, NULL);
    g_signal_add_emission_hook(g_signal_lookup("drag-end", GTK_TYPE_DRAG_SOURCE), 0, on_drag_end_hook, NULL, NULL);
}

static gboolean
start_preloaded(gpointer data)
{
    (void)data;
    start();
    return G_SOURCE_REMOVE;
}

__attribute__((constructor)) static void
preload(void)
{
    const char *exe;
    char buffer[PATH_MAX];
    ssize_t n;

    if (!g_getenv("ELAVO_APPLICATIONS_PRELOAD"))
        return;
    n = readlink("/proc/self/exe", buffer, sizeof buffer - 1);
    if (n <= 0)
        return;
    buffer[n] = '\0';
    exe = strrchr(buffer, '/');
    if (g_strcmp0(exe ? exe + 1 : buffer, "nautilus") == 0)
        g_idle_add(start_preloaded, NULL);
}

G_MODULE_EXPORT void
nautilus_module_initialize(GTypeModule *module)
{
    (void)module;
    /* A preloaded copy under test replaces the installed one. */
    if (!g_getenv("ELAVO_APPLICATIONS_PRELOAD"))
        start();
}

G_MODULE_EXPORT void
nautilus_module_shutdown(void)
{
}

G_MODULE_EXPORT void
nautilus_module_list_types(const GType **types, int *n_types)
{
    *types = NULL;
    *n_types = 0;
}
