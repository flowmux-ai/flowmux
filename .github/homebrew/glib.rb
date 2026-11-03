# SPDX-License-Identifier: GPL-3.0-or-later
# CI runtime only: GTK's bottled introspection data does not need regenerating.
class Glib < Formula
  desc "GLib runtime for Flowmux native CI"
  homepage "https://docs.gtk.org/glib/"
  url "https://download.gnome.org/sources/glib/2.88/glib-2.88.4.tar.xz"
  sha256 "25fd169f3b2f91fe2391ad297883ce0fcf30c69fc789ffd1c9672d1332b1c5a8"
  license "LGPL-2.1-or-later"

  depends_on "meson" => :build
  depends_on "ninja" => :build
  depends_on "pkgconf" => :build
  depends_on "gettext"
  depends_on "pcre2"
  uses_from_macos "libffi"

  def install
    system "meson", "setup", "build", *std_meson_args,
           "-Dintrospection=disabled", "-Dtests=false", "-Ddtrace=disabled",
           "-Dgio_module_dir=#{HOMEBREW_PREFIX}/lib/gio/modules"
    system "meson", "compile", "-C", "build"
    system "meson", "install", "-C", "build"

    gettext = Formula["gettext"]
    inreplace lib/"pkgconfig/glib-2.0.pc" do |s|
      s.gsub! "-lintl", "-L#{gettext.opt_lib} -lintl"
      s.gsub! "Cflags:", "Cflags: -I#{gettext.opt_include}"
    end
  end
end
