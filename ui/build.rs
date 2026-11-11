fn main() {
    // ComponentContainer (encaixar a tela de um plugin, interpretada em tempo de execução)
    // ainda é recurso experimental do Slint 1.18
    // SAFETY: o build script tem uma thread só; nada lê o ambiente ao mesmo tempo
    unsafe { std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1") };
    // live-preview: o gerador lê esta variável e, em vez da interface compilada, gera uma
    // casca que interpreta hub.slint em tempo de execução e recarrega ao salvar
    if std::env::var_os("CARGO_FEATURE_LIVE_PREVIEW").is_some() {
        // SAFETY: idem
        unsafe { std::env::set_var("SLINT_LIVE_PREVIEW", "1") };
    }
    // dev-mcp: ids dos elementos no código gerado, para o MCP achá-los (`find_elements_by_id`)
    // (sem o recurso, vale o `SLINT_EMIT_DEBUG_INFO` do ambiente, que os testes de UI usam)
    let mut config = slint_build::CompilerConfiguration::new();
    if std::env::var_os("CARGO_FEATURE_DEV_MCP").is_some() {
        config = config.with_debug_info(true);
    }
    // hub.slint importa as telas dos mini-apps (../miniapps/*/view.slint); o slint-build
    // avisa o cargo de cada arquivo lido, então mudar qualquer um recompila este crate.
    slint_build::compile_with_config("hub.slint", config).expect("falha ao compilar a UI Slint");
}
