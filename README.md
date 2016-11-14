TeamSpeak3 Text to Speech Plugin
================================

This plugin adds the possibility to have text to speech on non-windows systems,
which is not working by default.

The plugin is built using the [TS3Plugin](https://github.com/Flakebi/rust-ts3plugin) library.

Dependencies
-----------
 - [eSpeak](http://espeak.sourceforge.net/)
 - aplay

It should be fairly simple to adjust the `tts` function to get the plugin working
with other text to speech software or on windows.

Usage
-----

Compile the plugin using `cargo build` (optionally with `--release`) and put
the resulting library into your TeamSpeak plugin folder.

License
-------
This project is licensed under the GPLv2 or later license. The full license can be found in the `LICENSE` file.
