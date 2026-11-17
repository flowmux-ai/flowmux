// SPDX-License-Identifier: GPL-3.0-or-later
// Inject into an isolated Flowmux to exercise GTK's internal preload calls.
#include <gtk/gtk.h>

static GThread *lookup_thread;
static gint released;
static gint worker_releases;

static void
icon_released (gpointer data, GObject *object)
{
  (void) data;
  (void) object;
  if (g_thread_self () != lookup_thread)
    g_atomic_int_inc (&worker_releases);
  g_atomic_int_inc (&released);
}

static gboolean
run_probe (gpointer data)
{
  (void) data;
  if (!gtk_is_initialized ())
    return G_SOURCE_CONTINUE;
  g_assert_cmpuint (gtk_get_major_version (), ==, 4);
  g_assert_cmpuint (gtk_get_minor_version (), ==, 14);
  g_assert_cmpuint (gtk_get_micro_version (), ==, 5);
  lookup_thread = g_thread_self ();
  GtkIconTheme *theme = gtk_icon_theme_new ();
  gtk_icon_theme_set_theme_name (theme, "Adwaita");
  g_assert_true (gtk_icon_theme_has_icon (theme, "folder"));
  for (int i = 0; i < 10000; i++)
    {
      GtkIconPaintable *icon = gtk_icon_theme_lookup_icon (
          theme, "folder", NULL, 129, 1, GTK_TEXT_DIR_NONE, GTK_ICON_LOOKUP_PRELOAD);
      g_assert_nonnull (icon);
      g_object_weak_ref (G_OBJECT (icon), icon_released, NULL);
      g_object_unref (icon);
      while (g_main_context_pending (NULL))
        g_main_context_iteration (NULL, FALSE);
      // Every cold preload must finish and release on the lookup thread.
      g_assert_cmpint (g_atomic_int_get (&worker_releases), ==, 0);
      g_assert_cmpint (g_atomic_int_get (&released), ==, i + 1);
    }
  g_object_unref (theme);
  g_print ("FLOWMUX_GTK_ICON_PRELOAD_OK: 10000 preloads, zero worker releases\n");
  return G_SOURCE_REMOVE;
}

__attribute__((constructor)) static void
install_probe (void)
{
  char *exe = g_file_read_link ("/proc/self/exe", NULL);
  if (exe && g_str_has_suffix (exe, "/flowmux"))
    g_timeout_add (1500, run_probe, NULL);
  g_free (exe);
}
