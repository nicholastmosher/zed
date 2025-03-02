mod extension_lsp_adapter;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use extension::{ExtensionGrammarProxy, ExtensionHostProxy, ExtensionLanguageProxy};
use gpui::App;
use language::{
    GlobalLanguageRegistry, LanguageMatcher, LanguageName, LanguageRegistry, LoadedLanguage,
};

pub fn init(cx: &mut App) {
    let language_registry = cx.global::<GlobalLanguageRegistry>().0.clone();
    let language_server_registry_proxy = LanguageServerRegistryProxy { language_registry };
    let extension_host_proxy = ExtensionHostProxy::default_global(cx);
    extension_host_proxy.register_grammar_proxy(language_server_registry_proxy.clone());
    extension_host_proxy.register_language_proxy(language_server_registry_proxy.clone());
    extension_host_proxy.register_language_server_proxy(language_server_registry_proxy);
}

#[derive(Clone)]
struct LanguageServerRegistryProxy {
    language_registry: Arc<LanguageRegistry>,
}

impl ExtensionGrammarProxy for LanguageServerRegistryProxy {
    fn register_grammars(&self, grammars: Vec<(Arc<str>, PathBuf)>) {
        self.language_registry.register_wasm_grammars(grammars)
    }
}

impl ExtensionLanguageProxy for LanguageServerRegistryProxy {
    fn register_language(
        &self,
        language: LanguageName,
        grammar: Option<Arc<str>>,
        matcher: LanguageMatcher,
        hidden: bool,
        load: Arc<dyn Fn() -> Result<LoadedLanguage> + Send + Sync + 'static>,
    ) {
        self.language_registry
            .register_language(language, grammar, matcher, hidden, load);
    }

    fn remove_languages(
        &self,
        languages_to_remove: &[LanguageName],
        grammars_to_remove: &[Arc<str>],
    ) {
        self.language_registry
            .remove_languages(&languages_to_remove, &grammars_to_remove);
    }
}
