# Temas, visuais e fundos por arquivo

Exemplos para personalizar o MiniUtils sem recompilar:

| Pasta | O que é | Exemplo |
|---|---|---|
| `temas/` | cores de um tema de app (cartão e mascote) | [`temas/oceano.toml`](temas/oceano.toml) |
| `visuais/` | visual da janela (moldura, fontes, cores, fundo) | [`visuais/oceano.toml`](visuais/oceano.toml) |
| `fundos/` | fundo do painel, em camadas (algumas animadas) | [`fundos/bolhas.toml`](fundos/bolhas.toml) |

Para usar, copie as três pastas para a pasta de dados do MiniUtils (`miniutils --pastas`
mostra onde: `~/.local/share/miniutils/` no Linux, `%APPDATA%\miniutils\` no Windows) e
clique em **Configurações › Aparência › Recarregar**. O visual "Oceano" aparece em
**Visual da janela**.

O formato de cada arquivo, os campos e as camadas estão no
[DESIGN.md › Temas, visuais e fundos por arquivo](../../DESIGN.md#temas-visuais-e-fundos-por-arquivo).
