# mosaic

A window tiling program. Used to add light tiling facilities to your not-tiling window manager.

## quick start

Install Rust: https://rustup.rs/

Install mosaic:

```
$ cargo install --git https://github.com/robn/mosaic.git
```

Set up some keybindings. Example for [Xfce](https://xfce.org/):

```
$ xfconf-query -c xfce4-keyboard-shortcuts -p '/commands/custom/<Primary><Super>Left' -s 'mosaic --active --horiz left --vert full'
$ xfconf-query -c xfce4-keyboard-shortcuts -p '/commands/custom/<Primary><Super>Right' -s 'mosaic --active --horiz right --vert full'
$ xfconf-query -c xfce4-keyboard-shortcuts -p '/commands/custom/<Primary><Super>Up' -s 'mosaic --active --horiz current --vert top'
$ xfconf-query -c xfce4-keyboard-shortcuts -p '/commands/custom/<Primary><Super>Down' -s 'mosaic --active --horiz current --vert bottom'
```
