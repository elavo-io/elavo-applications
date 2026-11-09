/* The Applications folder in Files.
 *
 * - Opening an entry (double-click, Enter) starts the application instead of
 *   showing the directory. Nautilus 50 opens folders through internal calls,
 *   so the module watches each window slot's location: when it goes from a
 *   folder into an .eapp entry in that folder, it starts the entry and goes
 *   back. A copy that lives outside the Applications folders is opened with
 *   elavo-applications-ctl open: a Flatpak whose app is gone is copied back
 *   in and installed, and the original copy stays. Opening an entry in a
 *   new tab or window shows its contents. Right-click Show Contents opens the
 *   folder in this window and does not start the application.
 * - A copied bundle keeps the icon file inside it. Files' stored icon is a
 *   path, and that path dies when the original entry is uninstalled, so the
 *   module points the copy at its own icon.
 * - Move to Trash and Delete refuse an entry whose application is running.
 *   The view's actions are replaced by wrappers that check first and then
 *   call Nautilus's own action. Emission hooks cannot stop a signal.
 * - /Applications is on /var, which GIO will not trash. Move to Trash and
 *   Delete there ask the service (elavo-applications-ctl remove) and leave
 *   the entry in place until Uninstall. The home folder still uses Trash.
 * - A drag that starts on an entry in an Applications folder never moves or
 *   copies the install. Wayland has no link action, so the drag offers a
 *   copy. Dropped in a Files window, on a folder, a sidebar place, or the
 *   path bar, the entry is swapped for a shortcut file and Nautilus copies
 *   that; inside an Applications folder the drop does nothing. A bundle that
 *   is already a copy outside those folders is the bundle: dropping it into
 *   Applications copies the directory, and dropping it anywhere else copies
 *   the directory too. The Dock pins an install and the desktop makes its
 *   own shortcut.
 * - Item labels leave out the .eapp suffix.
 * - A drop into a shared folder this account cannot write asks for an
 *   administrator's password (elavo-applications-ctl install --shared).
 *
 * Nautilus loads this from <libdir>/nautilus/extensions-4. For testing on a
 * read-only /usr it also starts when preloaded:
 *   ELAVO_APPLICATIONS_PRELOAD=1 LD_PRELOAD=$PWD/libelavo-applications.so nautilus
 */

#include <dlfcn.h>
#include <fcntl.h>
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
#define SHOW_CONTENTS "elavo-applications-show-contents"
#define CONTENTS_MARK "elavo-show-contents"
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

static GtkSelectionModel *
selection_model(GtkWidget *view)
{
    GtkWidget *list = find_descendant(view, GTK_TYPE_GRID_VIEW);

    if (list)
        return gtk_grid_view_get_model(GTK_GRID_VIEW(list));
    if ((list = find_descendant(view, GTK_TYPE_COLUMN_VIEW)))
        return gtk_column_view_get_model(GTK_COLUMN_VIEW(list));
    return NULL;
}

/* The paths of the selected items in a files view. */
static GPtrArray *
selected_paths(GtkWidget *view)
{
    GPtrArray *paths = g_ptr_array_new_with_free_func(g_free);
    GtkSelectionModel *model = selection_model(view);
    GtkBitset *selection;
    GtkBitsetIter iter;
    guint position;

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

/* path is a bundle directly in /Applications. GIO cannot trash that mount. */
static gboolean
in_shared(const char *path)
{
    char *resolved = real(path);
    char *shared = shared_dir();
    char *parent = NULL;
    gboolean result = FALSE;

    if (resolved && shared && is_bundle(resolved)) {
        parent = g_path_get_dirname(resolved);
        result = g_strcmp0(parent, shared) == 0;
    }
    g_free(parent);
    g_free(resolved);
    g_free(shared);
    return result;
}

/* Asks the service. The directory stays until the person chooses Uninstall. */
static gboolean
ask_remove(const char *path, char **message)
{
    const char *argv[] = { ctl(), "remove", path, NULL };
    char *err_out = NULL;
    int status = 0;
    GError *error = NULL;

    if (!g_spawn_sync(NULL, (char **)argv, NULL, G_SPAWN_SEARCH_PATH | G_SPAWN_STDOUT_TO_DEV_NULL, NULL, NULL, NULL,
                      &err_out, &status, &error)) {
        *message = g_strdup(error->message);
        g_error_free(error);
        return FALSE;
    }
    if (!g_spawn_check_wait_status(status, &error)) {
        if (err_out && g_strstrip(err_out)[0] != '\0') {
            *message = err_out;
            g_clear_error(&error);
        } else {
            *message = g_strdup(error ? error->message : "The application could not be removed");
            g_clear_error(&error);
            g_free(err_out);
        }
        return FALSE;
    }
    g_free(err_out);
    return TRUE;
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

static gboolean in_applications(const char *resolved);

static void
tell_open_failed(GtkWidget *slot, const char *bundle, const char *detail)
{
    char *name = entry_name(bundle);
    char *heading = g_strdup_printf("“%s” could not be opened", name);

    tell(slot, heading, detail && *detail ? detail : "The application could not be opened");
    g_free(heading);
    g_free(name);
}

static gboolean
launch_and_return(gpointer data)
{
    Opened *opened = data;
    char *desktop = g_build_filename(opened->bundle, "launch.desktop", NULL);
    GDesktopAppInfo *info = NULL;
    GError *error = NULL;

    gtk_widget_activate_action(opened->slot, "slot.back", NULL);
    /* A copy outside Applications is not the install. Opening it puts a
     * missing Flatpak back, or launches the copy. flatpak run of an
     * uninstalled app exits immediately and still counts as launched. */
    if (!in_applications(opened->bundle)) {
        const char *argv[] = { ctl(), "open", opened->bundle, NULL };
        char *err = NULL;
        int status = 0;

        if (!g_spawn_sync(NULL, (char **)argv, NULL, G_SPAWN_SEARCH_PATH, NULL, NULL, NULL, &err, &status, NULL) ||
            !g_spawn_check_wait_status(status, NULL))
            tell_open_failed(opened->slot, opened->bundle, err);
        g_free(err);
        g_free(desktop);
        g_object_unref(opened->slot);
        g_free(opened->bundle);
        g_free(opened);
        return G_SOURCE_REMOVE;
    }
    info = g_desktop_app_info_new_from_filename(desktop);
    if (info) {
        GdkAppLaunchContext *context = gdk_display_get_app_launch_context(gtk_widget_get_display(opened->slot));
        if (!g_app_info_launch(G_APP_INFO(info), NULL, G_APP_LAUNCH_CONTEXT(context), &error)) {
            tell_open_failed(opened->slot, opened->bundle, error->message);
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
    /* Show Contents sets this so the folder stays open. A double-click does not. */
    if (resolved && is_bundle(resolved) && g_strcmp0(g_object_get_data(object, SHOW_CONTENTS), resolved) == 0)
        g_object_set_data(object, SHOW_CONTENTS, NULL);
    else if (resolved && is_bundle(resolved) && previous && g_strcmp0(previous, parent) == 0) {
        Opened *opened = g_new0(Opened, 1);
        opened->slot = g_object_ref(slot);
        opened->bundle = g_strdup(resolved);
        g_idle_add(launch_and_return, opened);
    } else if (g_object_get_data(object, SHOW_CONTENTS))
        g_object_set_data(object, SHOW_CONTENTS, NULL);
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

static void unselect_shared(GtkWidget *view);

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
            gboolean shared = in_shared(path);
            g_message("elavo applications: %s is running; not removing it", path);
            char *heading = g_strdup_printf("“%s” is open", name);
            char *body = g_strdup_printf(shared ? "Quit %s, then delete it." : "Quit %s, then move it to the Trash.",
                                         name);
            tell(wrapped->view, heading, body);
            g_free(body);
            g_free(heading);
            g_free(name);
            g_ptr_array_unref(paths);
            return;
        }
    }
    gboolean any_shared = FALSE;
    gboolean any_other = FALSE;
    for (i = 0; i < paths->len; i++) {
        const char *path = g_ptr_array_index(paths, i);
        char *error = NULL;

        if (!in_shared(path)) {
            any_other = TRUE;
            continue;
        }
        if (!ask_remove(path, &error)) {
            tell(wrapped->view, "The application could not be removed", error);
            g_free(error);
            g_ptr_array_unref(paths);
            return;
        }
        any_shared = TRUE;
    }
    g_ptr_array_unref(paths);
    /* Nautilus would delete the shared entry itself. Drop it from the
     * selection so the original action only sees the other files. */
    if (any_shared && any_other)
        unselect_shared(wrapped->view);
    if (!any_shared || any_other)
        g_action_activate(wrapped->original, parameter);
}

static void
unselect_shared(GtkWidget *view)
{
    GtkSelectionModel *model = selection_model(view);
    GtkBitset *selection;
    GtkBitsetIter iter;
    guint position;

    if (!model)
        return;
    selection = gtk_selection_model_get_selection(model);
    if (gtk_bitset_iter_init_first(&iter, selection, &position)) {
        do {
            GObject *item = g_list_model_get_item(G_LIST_MODEL(model), position);
            char *path = item_path(item);
            if (path && in_shared(path))
                gtk_selection_model_unselect_item(model, position);
            g_free(path);
            g_clear_object(&item);
        } while (gtk_bitset_iter_next(&iter, &position));
    }
    gtk_bitset_unref(selection);
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

/* The one selected bundle, or NULL when the selection is anything else. */
static char *
single_bundle(GtkWidget *view)
{
    GPtrArray *paths = selected_paths(view);
    char *bundle = NULL;

    if (paths->len == 1) {
        char *resolved = real(g_ptr_array_index(paths, 0));

        if (resolved && is_bundle(resolved))
            bundle = resolved;
        else
            g_free(resolved);
    }
    g_ptr_array_unref(paths);
    return bundle;
}

static void
on_show_contents(GSimpleAction *action, GVariant *parameter, gpointer data)
{
    GtkWidget *view = data;
    GtkWidget *slot = ancestor(view, slot_type);
    char *bundle = single_bundle(view);
    char *uri;

    (void)action;
    (void)parameter;
    if (!slot || !bundle) {
        g_free(bundle);
        return;
    }
    uri = g_filename_to_uri(bundle, NULL, NULL);
    g_object_set_data_full(G_OBJECT(slot), SHOW_CONTENTS, bundle, g_free);
    if (!uri || !gtk_widget_activate_action(slot, "slot.open-location", "s", uri))
        g_object_set_data(G_OBJECT(slot), SHOW_CONTENTS, NULL);
    g_free(uri);
}

static gboolean
model_has_action(GMenuModel *model, const char *action)
{
    int i;
    int n = g_menu_model_get_n_items(model);

    for (i = 0; i < n; i++) {
        char *value = NULL;
        GMenuModel *section = g_menu_model_get_item_link(model, i, G_MENU_LINK_SECTION);
        GMenuModel *submenu = g_menu_model_get_item_link(model, i, G_MENU_LINK_SUBMENU);
        gboolean found = FALSE;

        if (g_menu_model_get_item_attribute(model, i, "action", "s", &value) && g_strcmp0(value, action) == 0)
            found = TRUE;
        g_free(value);
        if (!found && section)
            found = model_has_action(section, action);
        if (!found && submenu)
            found = model_has_action(submenu, action);
        g_clear_object(&section);
        g_clear_object(&submenu);
        if (found)
            return TRUE;
    }
    return FALSE;
}

static gboolean
is_contents_section(GMenuModel *model, int index)
{
    char *mark = NULL;
    gboolean ours;

    ours = g_menu_model_get_item_attribute(model, index, "elavo", "s", &mark) && g_strcmp0(mark, CONTENTS_MARK) == 0;
    g_free(mark);
    return ours;
}

/* The top-level section that contains `action`, or -1. */
static int
section_with_action(GMenuModel *menu, const char *action)
{
    int i;
    int n = g_menu_model_get_n_items(menu);

    for (i = 0; i < n; i++) {
        GMenuModel *section = g_menu_model_get_item_link(menu, i, G_MENU_LINK_SECTION);
        gboolean found = section && model_has_action(section, action);

        g_clear_object(&section);
        if (found)
            return i;
    }
    return -1;
}

/* Nautilus keeps the selection menu on the view. GTK has no getter for it, so
 * the field is found the same way as the view's actions. */
static GMenu *
selection_menu(GObject *view)
{
    static gsize offset;
    GArray *maps;
    gsize at;
    GMenu *found = NULL;

    if (offset) {
        gpointer candidate = *(gpointer *)((char *)view + offset);

        if (G_IS_MENU(candidate) && model_has_action(G_MENU_MODEL(candidate), "view.cut") &&
            model_has_action(G_MENU_MODEL(candidate), "view.open-item-new-tab"))
            return G_MENU(candidate);
        offset = 0;
    }
    maps = load_maps();
    for (at = sizeof(gpointer); at < 8192; at += sizeof(gpointer)) {
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
        if (!G_IS_MENU(candidate))
            continue;
        if (!model_has_action(G_MENU_MODEL(candidate), "view.cut") ||
            !model_has_action(G_MENU_MODEL(candidate), "view.open-item-new-tab"))
            continue;
        offset = at;
        found = G_MENU(candidate);
        break;
    }
    g_array_unref(maps);
    return found;
}

/* One section of our own, after Open and before Cut. Nothing else in the menu
 * is added, removed, or rebuilt: the other rows keep the icons Nautilus gave
 * them. This runs when the popover is parented, before it reads the model. */
static void
place_show_contents(GtkWidget *view)
{
    GMenu *menu = selection_menu(G_OBJECT(view));
    char *bundle;
    int cut;
    int i;
    GMenu *section;
    GMenuItem *holder;

    if (!menu)
        return;
    for (i = g_menu_model_get_n_items(G_MENU_MODEL(menu)) - 1; i >= 0; i--) {
        if (is_contents_section(G_MENU_MODEL(menu), i))
            g_menu_remove(menu, i);
    }
    bundle = single_bundle(view);
    if (!bundle)
        return;
    g_free(bundle);
    cut = section_with_action(G_MENU_MODEL(menu), "view.cut");
    if (cut < 0)
        return;
    section = g_menu_new();
    g_menu_append(section, "Show Contents", "view.show-contents");
    holder = g_menu_item_new_section(NULL, G_MENU_MODEL(section));
    g_menu_item_set_attribute(holder, "elavo", "s", CONTENTS_MARK);
    g_menu_insert_item(menu, cut, holder);
    g_object_unref(holder);
    g_object_unref(section);
}

static void
watch_view(GtkWidget *view)
{
    GActionMap *map;
    GSimpleAction *show_contents;

    if (g_object_get_data(G_OBJECT(view), WATCHED))
        return;
    g_object_set_data(G_OBJECT(view), WATCHED, GINT_TO_POINTER(1));
    map = view_group(G_OBJECT(view), "move-to-trash");
    if (!map) {
        g_warning("elavo applications: no view actions found");
        return;
    }
    show_contents = g_simple_action_new("show-contents", NULL);
    g_signal_connect(show_contents, "activate", G_CALLBACK(on_show_contents), view);
    g_action_map_add_action(map, G_ACTION(show_contents));
    g_object_unref(show_contents);
    /* Nautilus 50's names. /Applications cannot be trashed, so there Nautilus
     * offers only the permanent deletes. */
    wrap_action(map, "move-to-trash", view);
    wrap_action(map, "delete-permanently-shortcut", view);
    wrap_action(map, "delete-permanently-menu-item", view);
    wrap_action(map, "permanent-delete-permanently-menu-item", view);
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

static char *
path_from_uri(const char *uri)
{
    GFile *file;
    char *path;

    if (!uri)
        return NULL;
    file = g_file_new_for_uri(uri);
    path = g_file_get_path(file);
    g_object_unref(file);
    return path;
}

/* A sidebar row keeps its folder in the "uri" property. */
static char *
row_folder(GtkWidget *row)
{
    char *uri = NULL;
    char *path;

    if (!row || !g_object_class_find_property(G_OBJECT_GET_CLASS(row), "uri"))
        return NULL;
    g_object_get(row, "uri", &uri, NULL);
    path = path_from_uri(uri);
    g_free(uri);
    return path;
}

/* The folder a drop lands in. The sidebar's drop target is the list, so the
 * place is the row under the pointer, not an ancestor. The path bar keeps
 * its path private. A sidebar drop is never the folder the window is showing. */
static char *
drop_folder(GtkWidget *widget, double y)
{
    GType row_type = g_type_from_name("NautilusSidebarRow");
    GtkWidget *row;
    char *path;

    if (view_cell_type && g_type_check_instance_is_a((GTypeInstance *)widget, view_cell_type)) {
        path = cell_path(widget);
        if (path && g_file_test(path, G_FILE_TEST_IS_DIR))
            return path;
        g_free(path);
    }
    row = ancestor(widget, row_type);
    if (!row && GTK_IS_LIST_BOX(widget))
        row = GTK_WIDGET(gtk_list_box_get_row_at_y(GTK_LIST_BOX(widget), y));
    if (row && (!row_type || g_type_check_instance_is_a((GTypeInstance *)row, row_type))) {
        path = row_folder(row);
        if (path)
            return path;
    }
    if (ancestor(widget, sidebar_type) ||
        (sidebar_type && g_type_check_instance_is_a((GTypeInstance *)widget, sidebar_type)))
        return NULL;
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
on_drop(GtkDropTarget *target, const GValue *value, double y, GValue *replacement)
{
    GtkWidget *widget = gtk_event_controller_get_widget(GTK_EVENT_CONTROLLER(target));
    char *folder = drop_folder(widget, y);
    char *resolved = real(folder);
    char *shared = shared_dir();
    gboolean handled = FALSE;
    gboolean bundles = FALSE;
    GSList *files, *l, *swapped = NULL;

    if (!G_VALUE_HOLDS(value, GDK_TYPE_FILE_LIST))
        goto out;
    files = gdk_file_list_get_files(g_value_get_boxed(value));

    /* An install dragged out of Applications becomes a shortcut. A drop of
     * that install back into Applications does nothing. A bundle that is
     * already a copy is left as the directory, so a drop into Applications
     * copies it and the service installs it. */
    for (l = files; l; l = l->next) {
        char *path = g_file_get_path(l->data);
        char *bundle = real(path);
        if (bundle && is_bundle(bundle)) {
            gboolean source_apps = in_applications(bundle);
            gboolean dest_apps = resolved && in_applications(resolved);
            bundles = TRUE;
            if (source_apps && !dest_apps) {
                GFile *shortcut = shortcut_for(bundle);
                if (shortcut)
                    swapped = g_slist_prepend(swapped, shortcut);
            } else if (!source_apps) {
                swapped = g_slist_prepend(swapped, g_object_ref(l->data));
            }
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
    if (on_drop(target, value, y, &replacement))
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

/* ---- Icons on copies ---- */

/* The picture that travels inside the bundle, in the order the service uses. */
static const char *BUNDLE_ICONS[] = {
    "custom-icon.png", "custom-icon.svg", "custom-icon.webp",
    "standard-icon.png", "icon.svg", "icon.png", NULL
};

static char *
bundle_icon(const char *bundle)
{
    guint i;

    for (i = 0; BUNDLE_ICONS[i]; i++) {
        char *path = g_build_filename(bundle, BUNDLE_ICONS[i], NULL);
        if (g_file_test(path, G_FILE_TEST_IS_REGULAR))
            return path;
        g_free(path);
    }
    return NULL;
}

static gboolean
icon_inside(const char *uri, const char *bundle)
{
    GFile *file;
    char *path;
    gsize n;
    gboolean ok = FALSE;

    if (!uri)
        return FALSE;
    file = g_file_new_for_uri(uri);
    path = g_file_get_path(file);
    n = strlen(bundle);
    if (path && strncmp(path, bundle, n) == 0 && path[n] == '/' && g_file_test(path, G_FILE_TEST_IS_REGULAR))
        ok = TRUE;
    g_free(path);
    g_object_unref(file);
    return ok;
}

/* Files stores a folder icon as a path. A copy outside the Applications
 * folders does not get the service's metadata, and the original path is gone
 * once that entry is uninstalled. */
static void
ensure_bundle_icon(const char *bundle)
{
    char *icon = bundle_icon(bundle);
    GFile *dir;
    GFileInfo *info;
    char *current = NULL;

    if (!icon)
        return;
    dir = g_file_new_for_path(bundle);
    info = g_file_query_info(dir, "metadata::custom-icon", G_FILE_QUERY_INFO_NONE, NULL, NULL);
    if (info)
        current = g_strdup(g_file_info_get_attribute_string(info, "metadata::custom-icon"));
    if (!icon_inside(current, bundle)) {
        GFile *icon_file = g_file_new_for_path(icon);
        char *uri = g_file_get_uri(icon_file);
        struct timespec times[2] = {
            { .tv_nsec = UTIME_OMIT },
            { .tv_nsec = UTIME_NOW },
        };

        g_file_set_attribute_string(dir, "metadata::custom-icon", uri, G_FILE_QUERY_INFO_NONE, NULL, NULL);
        g_file_set_attribute(dir, "metadata::custom-icon-name", G_FILE_ATTRIBUTE_TYPE_INVALID, NULL,
                             G_FILE_QUERY_INFO_NONE, NULL, NULL);
        utimensat(AT_FDCWD, bundle, times, 0);
        g_free(uri);
        g_object_unref(icon_file);
    }
    g_free(current);
    g_clear_object(&info);
    g_object_unref(dir);
    g_free(icon);
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
    else if (view_cell_type && g_type_check_instance_is_a((GTypeInstance *)widget, view_cell_type)) {
        char *path = cell_path(widget);
        char *resolved = real(path);
        /* Copies only. An entry in the Applications folders keeps the icon
         * the service set, including an Elavo theme icon. */
        if (resolved && is_bundle(resolved) && !in_applications(resolved))
            ensure_bundle_icon(resolved);
        g_free(resolved);
        g_free(path);
        watch_drop_targets(widget);
    } else if (ancestor(widget, sidebar_type) || ancestor(widget, path_bar_type))
        watch_drop_targets(widget);
    return TRUE;
}

static void (*popover_root_original)(GtkWidget *widget);

/* Runs while the menu popover is being parented, before Nautilus gives it the
 * menu. The rows, and their icons, are built once from the finished model. */
static void
popover_root(GtkWidget *widget)
{
    GtkWidget *parent;

    if (popover_root_original)
        popover_root_original(widget);
    if (!GTK_IS_POPOVER_MENU(widget) || gtk_popover_menu_get_menu_model(GTK_POPOVER_MENU(widget)))
        return;
    if (!files_view_type)
        files_view_type = g_type_from_name("NautilusFilesView");
    parent = gtk_widget_get_parent(widget);
    if (parent && files_view_type && g_type_check_instance_is_a((GTypeInstance *)parent, files_view_type))
        place_show_contents(parent);
}

static void
start(void)
{
    GtkWidgetClass *popover;

    if (started)
        return;
    started = TRUE;
    file_info_get_location = (FileInfoGetLocation)dlsym(RTLD_DEFAULT, "nautilus_file_info_get_location");
    /* Signals exist once their class is initialized. */
    g_type_class_ref(GTK_TYPE_WIDGET);
    g_type_class_ref(GTK_TYPE_DRAG_SOURCE);
    popover = GTK_WIDGET_CLASS(g_type_class_ref(GTK_TYPE_POPOVER_MENU));
    if (popover->root != popover_root) {
        popover_root_original = popover->root;
        popover->root = popover_root;
    }
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
