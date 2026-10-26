#!/bin/sh
# Installs (or updates) WordCraft IA for the current user, beside any official WordCraft.
#
# Copies wordcraft and wordcraft-cli from this folder to ~/.local/opt/wordcraft-ia and adds
# "WordCraft IA" to the applications menu (and the desktop, when there is one). No administrator
# rights are needed. An official WordCraft (deb, rpm, AppImage, Flatpak) lives elsewhere, so its
# updates never replace this copy (nor this one that). Settings, recent files and API keys are
# shared with the official app and kept on update.
#
# Run it from a terminal opened in this folder:  sh install-ia.sh
set -eu
APP=wordcraft
TITLE="WordCraft IA"
ID=wordcraft-ia
SRC=$(cd "$(dirname "$0")" && pwd)
DEST="$HOME/.local/opt/$ID"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"

for file in "$APP" "$APP-cli"; do
  if [ ! -f "$SRC/$file" ]; then
    echo "Falta $file junto a este instalador: descomprime todo el zip primero." >&2
    exit 1
  fi
done

# Linux lets a running program's file be replaced: an open WordCraft IA keeps working and uses
# the new version the next time it is opened.
mkdir -p "$DEST"
for file in "$APP" "$APP-cli" LEEME-IA.txt; do
  [ -f "$SRC/$file" ] || continue
  cp -f "$SRC/$file" "$DEST/.$file.update"
  mv -f "$DEST/.$file.update" "$DEST/$file"
done
chmod 755 "$DEST/$APP" "$DEST/$APP-cli"

mkdir -p "$DATA/icons/hicolor/256x256/apps" "$DATA/applications" "$HOME/.local/bin"
if [ -f "$SRC/$ID.png" ]; then
  cp -f "$SRC/$ID.png" "$DATA/icons/hicolor/256x256/apps/$ID.png"
fi
ln -sf "$DEST/$APP" "$HOME/.local/bin/$ID"

# The window identifies itself as the official app (ai.storyteller.$APP); StartupWMClass lets
# the dock show this entry's name and icon for it.
LAUNCHER="$DATA/applications/$ID.desktop"
cat > "$LAUNCHER" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$TITLE
GenericName=Procesador de textos
Comment=WordCraft con el asistente de IA (claves de API o tu plan de ChatGPT / Claude)
Exec="$DEST/$APP" %F
TryExec=$DEST/$APP
Path=$DEST
Icon=$ID
Terminal=false
StartupNotify=true
StartupWMClass=ai.storyteller.$APP
Categories=Office;WordProcessor;
Keywords=word;processor;document;docx;writer;text;ia;ai;asistente;
MimeType=application/vnd.openxmlformats-officedocument.wordprocessingml.document;application/vnd.ms-word.document.macroEnabled.12;application/vnd.openxmlformats-officedocument.wordprocessingml.template;application/vnd.ms-word.template.macroEnabled.12;application/msword;application/msword-template;application/vnd.oasis.opendocument.text;application/rtf;text/rtf;text/markdown;text/plain;
DESKTOP
chmod 644 "$LAUNCHER"

# A desktop icon too, when the session has a desktop folder (Ubuntu asks to trust it once).
DESKTOP_DIR=$(xdg-user-dir DESKTOP 2>/dev/null || true)
if [ -n "$DESKTOP_DIR" ] && [ "$DESKTOP_DIR" != "$HOME" ] && [ -d "$DESKTOP_DIR" ]; then
  cp -f "$LAUNCHER" "$DESKTOP_DIR/$ID.desktop"
  chmod 755 "$DESKTOP_DIR/$ID.desktop"
  gio set "$DESKTOP_DIR/$ID.desktop" metadata::trusted true 2>/dev/null || true
fi

update-desktop-database "$DATA/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$DATA/icons/hicolor" 2>/dev/null || true

echo
echo "$TITLE quedó instalado en $DEST."
echo "Ábrelo desde el menú de aplicaciones (busca «$TITLE») o con el comando $ID."
echo "El asistente está en la pestaña «Asistente» de la cinta."
