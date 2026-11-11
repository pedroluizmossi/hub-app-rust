fn main() {
    // ComponentContainer (encaixar a tela de um plugin, interpretada em tempo de execução)
    // ainda é recurso experimental do Slint 1.18
    // SAFETY: o build script tem uma thread só; nada lê o ambiente ao mesmo tempo
    unsafe { std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1") };
    // hub.slint importa as telas dos mini-apps (../miniapps/*/view.slint); o slint-build
    // avisa o cargo de cada arquivo lido, então mudar qualquer um recompila este crate.
    slint_build::compile("hub.slint").expect("falha ao compilar a UI Slint");
}
