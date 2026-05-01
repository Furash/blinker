# -*- mode: python ; coding: utf-8 -*-

import os

# PySide6 modules NOT used by blinker_ui.py.
# Used: QtCore, QtGui, QtNetwork (QLocalServer/Socket only), QtWidgets, QtSvg (icons).
EXCLUDE_PYSIDE_MODULES = [
    'PySide6.Qt3DAnimation', 'PySide6.Qt3DCore', 'PySide6.Qt3DExtras',
    'PySide6.Qt3DInput', 'PySide6.Qt3DLogic', 'PySide6.Qt3DRender',
    'PySide6.QtBluetooth', 'PySide6.QtCharts', 'PySide6.QtChartsQml',
    'PySide6.QtConcurrent', 'PySide6.QtDataVisualization',
    'PySide6.QtDataVisualizationQml', 'PySide6.QtDBus', 'PySide6.QtDesigner',
    'PySide6.QtHelp', 'PySide6.QtLocation', 'PySide6.QtMultimedia',
    'PySide6.QtMultimediaWidgets', 'PySide6.QtNfc', 'PySide6.QtOpenGL',
    'PySide6.QtOpenGLWidgets', 'PySide6.QtPdf', 'PySide6.QtPdfWidgets',
    'PySide6.QtPositioning', 'PySide6.QtPrintSupport', 'PySide6.QtQml',
    'PySide6.QtQuick', 'PySide6.QtQuick3D', 'PySide6.QtQuickControls2',
    'PySide6.QtQuickWidgets', 'PySide6.QtRemoteObjects', 'PySide6.QtScxml',
    'PySide6.QtSensors', 'PySide6.QtSerialBus', 'PySide6.QtSerialPort',
    'PySide6.QtSpatialAudio', 'PySide6.QtSql', 'PySide6.QtStateMachine',
    'PySide6.QtTest', 'PySide6.QtTextToSpeech', 'PySide6.QtUiTools',
    'PySide6.QtWebChannel', 'PySide6.QtWebEngineCore',
    'PySide6.QtWebEngineQuick', 'PySide6.QtWebEngineWidgets',
    'PySide6.QtWebSockets', 'PySide6.QtXml',
]

# Qt runtime DLLs / dirs to drop. PyInstaller's PySide6 hook bundles these
# even when the Python bindings are excluded above.
DROP_DLL_BASENAMES = {
    'qt6quick.dll', 'qt6qml.dll', 'qt6qmlmodels.dll', 'qt6qmlmeta.dll',
    'qt6qmlworkerscript.dll', 'qt6pdf.dll', 'qt6opengl.dll',
    'qt6virtualkeyboard.dll', 'opengl32sw.dll',
    # Duplicate openssl set (Python's _ssl). Keep the Qt -x64 copies.
    'libcrypto-3.dll', 'libssl-3.dll',
}

# Plugins to drop. Keep platforms/qwindows, styles/qmodernwindowsstyle,
# imageformats/{qico,qsvg}, iconengines/qsvgicon.
DROP_PLUGIN_FILES = {
    # tls (no SSL traffic, only QLocalServer/Socket)
    'plugins/tls/qcertonlybackend.dll',
    'plugins/tls/qopensslbackend.dll',
    'plugins/tls/qschannelbackend.dll',
    # alt platforms
    'plugins/platforms/qdirect2d.dll',
    'plugins/platforms/qminimal.dll',
    'plugins/platforms/qoffscreen.dll',
    # virtual keyboard, generic input, network info (unused by QLocalServer)
    'plugins/platforminputcontexts/qtvirtualkeyboardplugin.dll',
    'plugins/generic/qtuiotouchplugin.dll',
    'plugins/networkinformation/qnetworklistmanager.dll',
    # imageformats not needed (PNG built into QtGui; we keep ico+svg)
    'plugins/imageformats/qgif.dll',
    'plugins/imageformats/qicns.dll',
    'plugins/imageformats/qjpeg.dll',
    'plugins/imageformats/qpdf.dll',
    'plugins/imageformats/qtga.dll',
    'plugins/imageformats/qtiff.dll',
    'plugins/imageformats/qwbmp.dll',
    'plugins/imageformats/qwebp.dll',
}


def _norm(p):
    return p.replace('\\', '/').lower()


def _keep_binary(entry):
    dest = _norm(entry[0])
    base = os.path.basename(dest)
    if base in DROP_DLL_BASENAMES:
        return False
    # PySide6/plugins/<group>/<file>
    if 'pyside6/plugins/' in dest:
        # match the trailing "plugins/<group>/<file>" part
        tail = dest.split('pyside6/', 1)[1]
        if tail in DROP_PLUGIN_FILES:
            return False
    return True


def _keep_data(entry):
    dest = _norm(entry[0])
    # Drop all Qt translations except English.
    if 'pyside6/translations/' in dest:
        return dest.endswith('/qt_en.qm')
    return True


a = Analysis(
    ['blinker_ui.py'],
    pathex=[],
    binaries=[],
    datas=[('bootstrap.py', '.'), ('logo.png', '.'), ('icons', 'icons')],
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=EXCLUDE_PYSIDE_MODULES,
    noarchive=False,
    optimize=0,
)

a.binaries = TOC([b for b in a.binaries if _keep_binary(b)])
a.datas = TOC([d for d in a.datas if _keep_data(d)])

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='BlinkerUI',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=['logo.ico'],
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name='BlinkerUI',
)
