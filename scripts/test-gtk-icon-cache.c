// SPDX-License-Identifier: GPL-3.0-or-later
// Exercise the GTK dependency patch without changing the running Flowmux.
#include <gtk/gtk.h>

typedef struct {
  GMutex mutex;
  GCond condition;
  gboolean finalizing;
  gboolean resume;
} FinalizeGate;

static FinalizeGate *finalize_gate;
static GObject *finalizing_icon;
static void (*original_finalize) (GObject *);

static void
pause_finalization (GObject *object)
{
  FinalizeGate *gate = finalize_gate;
  const gint64 deadline = g_get_monotonic_time () + 5 * G_TIME_SPAN_SECOND;

  if (object == finalizing_icon)
    {
      g_assert_cmpuint (g_atomic_int_get (&object->ref_count), ==, 0);
      g_mutex_lock (&gate->mutex);
      gate->finalizing = TRUE;
      g_cond_signal (&gate->condition);
      while (!gate->resume)
        g_assert_true (g_cond_wait_until (&gate->condition, &gate->mutex, deadline));
      g_mutex_unlock (&gate->mutex);
    }
  original_finalize (object);
}

static gpointer
release_icon (gpointer icon)
{
  g_object_unref (icon);
  return NULL;
}

static GtkIconPaintable *
lookup (GtkIconTheme *theme, GtkIconLookupFlags flags)
{
  /* Sizes above 128 bypass GTK's additional strong-reference LRU. */
  GtkIconPaintable *icon = gtk_icon_theme_lookup_icon (
      theme, "image-missing", NULL, 129, 1, GTK_TEXT_DIR_NONE, flags);
  g_assert_nonnull (icon);
  return icon;
}

static void
finalize_race (void)
{
  GtkIconTheme *theme = gtk_icon_theme_new ();
  GtkIconPaintable *old = lookup (theme, 0);
  FinalizeGate gate = { 0 };
  GWeakRef released;
  GObjectClass *icon_class = g_type_class_ref (GTK_TYPE_ICON_PAINTABLE);
  const gint64 deadline = g_get_monotonic_time () + 5 * G_TIME_SPAN_SECOND;

  g_mutex_init (&gate.mutex);
  g_cond_init (&gate.condition);
  /* Pause at the finalizer boundary; weak notifications can run before zero. */
  original_finalize = icon_class->finalize;
  finalize_gate = &gate;
  finalizing_icon = G_OBJECT (old);
  icon_class->finalize = pause_finalization;
  GThread *thread = g_thread_new ("icon-finalize", release_icon, old);
  g_mutex_lock (&gate.mutex);
  while (!gate.finalizing)
    g_assert_true (g_cond_wait_until (&gate.condition, &gate.mutex, deadline));
  g_mutex_unlock (&gate.mutex);

  /* The old object's refcount is zero, but its cache entry still exists. */
  GtkIconPaintable *replacement = lookup (theme, 0);
  g_assert_true (replacement != old);
  g_mutex_lock (&gate.mutex);
  gate.resume = TRUE;
  g_cond_signal (&gate.condition);
  g_mutex_unlock (&gate.mutex);
  g_thread_join (thread);
  icon_class->finalize = original_finalize;
  g_type_class_unref (icon_class);

  /* Finalizing the old icon must not evict its replacement with the same key. */
  GtkIconPaintable *again = lookup (theme, 0);
  g_assert_true (again == replacement);
  g_object_unref (again);
  g_weak_ref_init (&released, replacement);
  g_object_unref (replacement);
  g_assert_null (g_weak_ref_get (&released));
  g_weak_ref_clear (&released);
  g_object_unref (theme);
  g_cond_clear (&gate.condition);
  g_mutex_clear (&gate.mutex);
}

static void
preload_stress (void)
{
  GtkIconTheme *theme = gtk_icon_theme_new ();
  gtk_icon_theme_set_theme_name (theme, "Adwaita");
  g_assert_true (gtk_icon_theme_has_icon (theme, "folder"));
  for (int i = 0; i < 100000; i++)
    {
      /* A real unloaded icon exercises GTask workers, unlike image-missing. */
      GtkIconPaintable *icon = gtk_icon_theme_lookup_icon (
          theme, "folder", NULL, 129, 1, GTK_TEXT_DIR_NONE, GTK_ICON_LOOKUP_PRELOAD);
      g_assert_nonnull (icon);
      g_object_unref (icon);
      g_main_context_iteration (NULL, FALSE);
    }
  g_object_unref (theme);
}

static void
small_icon_lifetime (void)
{
  GtkIconTheme *theme = gtk_icon_theme_new ();
  GtkIconPaintable *icon = gtk_icon_theme_lookup_icon (
      theme, "image-missing", NULL, 16, 1, GTK_TEXT_DIR_NONE, 0);
  GWeakRef released;
  g_assert_nonnull (icon);
  g_weak_ref_init (&released, icon);
  g_object_unref (icon);
  icon = g_weak_ref_get (&released);
  g_assert_nonnull (icon);
  g_object_unref (icon);
  gtk_icon_theme_set_theme_name (theme, "hicolor");
  g_assert_null (g_weak_ref_get (&released));
  g_weak_ref_clear (&released);
  g_object_unref (theme);
}

int
main (int argc, char **argv)
{
  g_test_init (&argc, &argv, NULL);
  gtk_init ();
  g_test_add_func ("/gtk/icon-cache/finalize-race", finalize_race);
  g_test_add_func ("/gtk/icon-cache/small-icon-lifetime", small_icon_lifetime);
  g_test_add_func ("/gtk/icon-cache/preload-stress", preload_stress);
  return g_test_run ();
}
