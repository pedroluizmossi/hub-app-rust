fn main() {
    // hub.slint importa as telas dos mini-apps (../miniapps/*/view.slint); o slint-build
    // avisa o cargo de cada arquivo lido, então mudar qualquer um recompila este crate.
    slint_build::compile("hub.slint").expect("falha ao compilar a UI Slint");
}
