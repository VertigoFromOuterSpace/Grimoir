# Grimoir — Plano de Arquitetura e v1

> Status: Fases 0, 1 e 2 concluídas no host; Fase 3 concluída no núcleo (busca no livro, anotações por região, Obsidian, estatísticas). CBZ tem leitor dedicado; EPUB usa fallback textual legível. Settings, empacotamento e CI adicionados. Pendências: renderer EPUB com WebKitGTK (dependência de sistema ausente), OCR (tesseract ausente), crop automático de margens e publicação no GitHub. Última atualização: 2026-11-05.
> Repositório público: https://github.com/VertigoFromOuterSpace/Grimoir
> Camada pessoal: local, fora do repositório público.

## 1. Visão

Grimoir é um leitor de documentos para Linux, **PDF-first**, leve e profundamente integrado ao desktop. Ele nasce como uma alternativa ao Foliate para o uso real do autor: uma biblioteca grande de mangá escaneado em PDF, alguns PDFs técnicos com texto, CBZ e EPUB ocasional.

Dois produtos que compartilham o mesmo núcleo:

1. **Grimoir** — aplicativo público, genérico, adaptável a qualquer Linux/Wayland (com fallback para X11), com tema embutido e API de tema estável.
2. **Grimoir Personal** — camada local, não publicada, que lê o tema do Hyprland, fontes, wallpaper e preferências da máquina e gera o tema/config consumido pelo Grimoir em tempo real.

## 2. Decisões consolidadas

| Tema | Decisão |
|---|---|
| Plataforma | Linux primeiro; Wayland/Hyprland como alvo principal; X11 como fallback |
| Stack | Rust + GTK4-rs/libadwaita; PDFium para PDF; WebKitGTK + foliate-js para EPUB |
| Prioridade | PDF (especialmente scans de mangá), depois EPUB/CBZ/texto |
| Biblioteca | Híbrida: coleções virtuais + pastas reais indexadas |
| Arquivos | Somente leitura: o app indexa, nunca move/renomeia/apaga sem ação explícita |
| Janelas | Biblioteca em uma janela; leitura em janela dedicada maximizada |
| Transparência | Dois modos com sliders: "Cinema" (barras laterais) e "Vidro" (papel também) |
| Scans no modo Vidro | Efeito **tinta no vidro**: papel branco some, traço/texto permanece opaco |
| Modos de leitura | Página única, página dupla, scroll vertical contínuo, RTL, LTR, zoom/pan livre, troca a qualquer momento |
| Anotações | Highlights de texto (PDF com camada/EPUB) e anotações por região (scans) |
| Busca | Dentro do livro; busca global fica para depois |
| Exportação | Obsidian: texto selecionado é sempre anexado ao fim de uma nota; página inteira vira imagem |
| Estatísticas | Tempo, páginas, sequência; dados locais |
| Dicionário/tradução | Adiado; planejado como "lente de seleção" com OCR sob demanda + tradução |
| TTS/sync/mobile | Fora do v1 |
| Licença | GPL-3.0-or-later |
| Idioma | Inglês na UI e na documentação pública |

## 3. Stack técnica

### 3.1 Por que GTK4-rs

- Wayland nativo, transparência por superfície e integração com o compositor.
- Widgets prontos para biblioteca (grid, listas, split view, busca, drag-and-drop).
- WebKitGTK embutido na mesma janela para o módulo EPUB.
- libadwaita fornece base de acessibilidade, IME, temas e preferências.
- Todo o domínio do app (biblioteca, PDF, anotações, banco, tema) é Rust; GTK/WebKit são bibliotecas de sistema, como no Foliate.

### 3.2 Componentes

| Componente | Responsabilidade |
|---|---|
| `grimoir` | Binário, ciclo de vida do app, single-instance, abertura de arquivos |
| `grimoir-ui` | Biblioteca, janela de leitura, widgets, atalhos, CSS |
| `grimoir-core` | SQLite, modelos, varredura de pastas, capas, progresso, anotações, estatísticas |
| `grimoir-pdf` | PDFium: render, texto, sumário, links, busca, seleção |
| `grimoir-epub` | WebKitGTK + foliate-js: render EPUB, CFI, seleção, temas |
| `grimoir-theme` | Schema de tema, parser, geração de CSS, watcher |
| `grimoir-personal` | Fora do repo público: lê Hyprland/GTK/wallpaper e gera `theme.json` |

### 3.3 Dependências principais

- `gtk4` + `libadwaita` (crates) — UI.
- `pdfium-render` + `libpdfium.so` (BSD) — PDF.
- `webkit2gtk` (crate) + foliate-js (MIT) — EPUB.
- `rusqlite` + `rusqlite_migration` — banco local.
- `serde`/`serde_json`/`toml` — config e tema.
- `notify` — watcher de tema e de pastas.
- `image` — capas, blur de wallpaper, pós-processamento do papel.
- `rayon` — paralelismo de indexação e keying.
- `tracing` — logs.
- `zbus` (opcional) — portal de tema quando disponível.

## 4. Estrutura do workspace

```
Grimoir/
├── Cargo.toml
├── crates/
│   ├── grimoir/
│   ├── grimoir-ui/
│   ├── grimoir-core/
│   ├── grimoir-pdf/
│   ├── grimoir-epub/
│   └── grimoir-theme/
├── assets/
│   ├── icons/
│   └── epub-viewer/        # foliate-js + bridge JS
├── docs/
│   ├── PLAN.md
│   └── THEME.md
├── scripts/
│   ├── fetch-pdfium.sh
│   └── dev.sh
├── .github/workflows/ci.yml
├── LICENSE
└── README.md
```

A camada pessoal fica em `../Grimoir-Personal/` (local, sem remote no GitHub). Ela pode depender de `grimoir-theme` por path local.

## 5. Arquitetura de execução

### 5.1 Inicialização

1. `grimoir` cria `GtkApplication` com `HANDLES_OPEN`.
2. Abre o banco em `~/.local/share/grimoir/library.db`, aplica migrações.
3. Carrega `~/.config/grimoir/config.toml` (opcional) e `~/.config/grimoir/theme.json`.
4. Se não houver tema externo, usa fallback embutido + portal `color-scheme`/accent.
5. Abre a janela da biblioteca.
6. `grimoir arquivo.pdf` abre direto a janela de leitura na instância existente.

### 5.2 Janela de leitura

- Sem decoração do servidor; barra flutuante que auto-oculta.
- Abre maximizada (Waybar visível) ou fullscreen (F11).
- Fundo da janela transparente; barras laterais desenhadas pelo app com alpha.
- Página desenhada em um widget customizado com `GtkSnapshot` + `GdkTexture`.
- Anotações e seleção desenhadas por cima, em coordenadas normalizadas.
- Cache LRU de páginas renderizadas; prefetch das páginas vizinhas.

### 5.3 Pipeline PDF

1. Abre o documento com PDFium em uma thread de render.
2. Renderiza a página em BGRA na resolução alvo (tela × zoom).
3. Aplica crop de margens (opcional) e o filtro de papel (modo Vidro).
4. Envia a textura para a UI; cacheia.
5. Texto, links, sumário e busca vêm das APIs de texto/bookmark do PDFium.
6. Ao mudar zoom, re-renderiza com debounce; mostra a textura escalada enquanto isso.

### 5.4 Pipeline EPUB

1. Extrai o EPUB em um diretório temporário ou serve via URI scheme local.
2. Carrega o visualizador foliate-js no WebKitGTK.
3. Ponte JS ↔ Rust por `webkit_web_view_evaluate_javascript` e script messages.
4. Tema do app é injetado como CSS; seleção/highlights usam CFI.
5. Conteúdo remoto bloqueado por padrão (privacidade); imagens internas liberadas.
6. WebKit só é inicializado quando o primeiro EPUB é aberto.

### 5.5 PDFium e threads

- `FPDF_InitLibrary` uma vez; chamadas protegidas por mutex global ou por documento.
- Uma thread de render por janela de leitura; indexação de capas em pool separado.
- Nunca bloquear o loop principal do GTK com render ou I/O.

## 6. Modelo de dados

```sql
books(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE NOT NULL,
  format TEXT NOT NULL,
  title TEXT,
  subtitle TEXT,
  authors TEXT,
  series TEXT,
  series_index REAL,
  language TEXT,
  publisher TEXT,
  published TEXT,
  description TEXT,
  isbn TEXT,
  page_count INTEGER,
  cover_path TEXT,
  file_size INTEGER,
  file_mtime INTEGER,
  file_hash TEXT,
  added_at INTEGER,
  last_opened_at INTEGER,
  favorite INTEGER DEFAULT 0,
  reading_status TEXT DEFAULT 'unread'
)

progress(
  book_id INTEGER PRIMARY KEY REFERENCES books(id),
  locator TEXT,            -- página PDF ou CFI EPUB
  page INTEGER,
  percent REAL,
  updated_at INTEGER
)

collections(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  parent_id INTEGER,
  kind TEXT NOT NULL,      -- 'virtual' | 'folder'
  path TEXT,               -- para kind='folder'
  sort_order INTEGER
)

book_collections(book_id, collection_id, PRIMARY KEY(book_id, collection_id))

annotations(
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL,
  kind TEXT NOT NULL,      -- 'highlight' | 'note' | 'region'
  page INTEGER,
  locator TEXT,
  color TEXT,
  text TEXT,
  note TEXT,
  rects_json TEXT,
  created_at INTEGER,
  updated_at INTEGER
)

obsidian_targets(
  book_id INTEGER PRIMARY KEY REFERENCES books(id),
  note_path TEXT NOT NULL,   -- absolute path to the cached destination note
  note_title TEXT,
  updated_at INTEGER
)

reading_sessions(
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL,
  started_at INTEGER,
  ended_at INTEGER,
  pages_read INTEGER,
  duration_seconds INTEGER
)

settings(key TEXT PRIMARY KEY, value TEXT)
```

- Busca de metadados com FTS5 (`books_fts`).
- Identidade de arquivo: `path` + `size` + `mtime`; hash parcial para detectar movidos.
- Migrações versionadas com `PRAGMA user_version`.

## 7. Biblioteca

### 7.1 Varredura

- Raízes configuráveis (ex.: `/path/to/books`).
- Profundidade configurável; sem seguir symlinks em loop.
- Ignorar: dotfiles, `.mctemp`, `.MEGABASTERD_CHUNKS_*`, `.part`, `.crdownload`, `.tmp`, `__MACOSX`.
- Watcher com `notify` + debounce para novos/removidos/alterados.
- Importação por drag-and-drop e "Adicionar pasta".

### 7.2 Capas

- PDF: primeira página não vazia, renderizada a ~400 px de largura.
- EPUB: imagem de capa do OPF; fallback primeira página.
- CBZ: primeira imagem.
- TXT/MD: capa gerada com gradiente + título.
- Cache em `~/.cache/grimoir/covers/<id>.webp`.
- Geração em background com fila e prioridade para itens visíveis.

### 7.3 UI

- Sidebar: Recentes, Todos, Favoritos, Pastas reais, Coleções, Formatos, Autores/Séries.
- Grid de capas com tamanho ajustável; lista compacta alternativa.
- Busca por título/autor/série; filtros e ordenação.
- Estante "Continuar lendo".
- Página de detalhes: capa, metadados editáveis, progresso, anotações, botão Ler.
- Agrupamento automático de séries/volumes por pasta e nome (`VOL. 01`, `Volume 1`, `v01`).

## 8. Leitura

### 8.1 Modos

- Página única.
- Página dupla (spread), com capa sozinha e ordem RTL/LTR.
- Scroll vertical contínuo (implementado depois do modo paginado, na mesma fase).
- Ajuste: página inteira, largura, altura, original.
- Zoom/pan livre; re-render em alta resolução.
- Crop automático de margens.
- Direção RTL/LTR alternável no leitor.
- Atalhos: setas, PageUp/Down, Home/End, +/-, F11, Ctrl+F, Ctrl+B, Esc.

### 8.2 Barra flutuante

- Voltar à biblioteca, sumário, anotações, busca, configurações, fullscreen, fechar.
- Auto-ocultar após 2 s; aparece ao mover o mouse para o topo.
- Arrastar a janela pela barra ou por área vazia.

### 8.3 Progresso

- PDF: página + offset de scroll.
- EPUB: CFI + porcentagem.
- Salvo ao virar página, ao fechar e a cada 30 s.
- "Continuar lendo" na biblioteca.

## 9. Transparência e tema

### 9.1 Modo Cinema

- Só as barras laterais/cromo são transparentes ou vidro.
- A página permanece 100% opaca.
- Sliders: opacidade do cromo (0–100%), intensidade do blur.
- Blur real: `backdrop-filter` do GTK 4.24 + `ext_background_effect` no Hyprland; fallback alpha + blur do compositor.

### 9.2 Modo Vidro

- O papel/branco da página também fica transparente; tinta/texto permanece opaco.
- Sliders: opacidade do papel (0–100%), força do key (0–100%), opacidade do cromo, blur.
- PDF com texto: mesmo filtro; texto e cores fortes preservados.
- EPUB: `background: transparent` no CSS; imagens opacas.
- Scans: efeito tinta no vidro; páginas coloridas podem ter key mais suave ou toggle por livro.

### 9.3 Algoritmo "tinta no vidro"

1. Renderiza a página em BGRA.
2. Estima a cor do papel (borda/histograma; normalmente branco).
3. Para cada pixel: distância à cor do papel → alpha.
   - Papel: alpha = `1 - paper_opacity`.
   - Tinta/texto: alpha = 1.
   - Bordas: transição suave (smoothstep).
4. Saturação alta (arte colorida) pode ser protegida para não sumir.
5. Cacheia a textura com key aplicado; recalcula ao mexer no slider (debounce).
6. Em páginas majoritariamente cinza (mangá), aplica por padrão; em páginas coloridas, modo suave.

### 9.4 Schema de tema

`~/.config/grimoir/theme.json`:

```json
{
  "version": 1,
  "name": "Grimoir Example",
  "mode": "dark",
  "colors": {
    "bg": "#080d0b",
    "surface": "#121a15",
    "surface_alt": "#1a241d",
    "text": "#dce9dd",
    "text_muted": "#9aa89c",
    "accent": "#b7c9b9",
    "border": "#263329",
    "shadow": "#040706",
    "danger": "#c08a72",
    "success": "#9bb88c",
    "warning": "#d4c79a"
  },
  "wallpaper": null,
  "fonts": {
    "ui": "IBM Plex Mono",
    "ui_size": 10,
    "reading": "Noto Serif",
    "reading_size": 11,
    "mono": "Hack"
  },
  "radius": 4,
  "blur": { "enabled": true, "size": 2, "passes": 2, "vibrancy": 0.4 },
  "opacity": {
    "chrome": 0.65,
    "sidebar": 0.55,
    "letterbox": 0.0,
    "paper": 1.0
  }
}
```

- O app gera CSS concreto a partir desses tokens e aplica com `CssProvider`.
- O app observa o arquivo e reaplica em tempo real (hot reload).
- Sem arquivo externo: fallback embutido + portal/gsettings.
- EPUB recebe os mesmos tokens via CSS injetado.

## 10. Camada pessoal (local)

### 10.1 Fonte de verdade

- `~/.local/state/hypr/theme.json` (tema canônico do `theme-apply`).
- Fallback: `~/.cache/wal/colors.json`, `~/.config/gtk-4.0/gtk.css`.
- `gsettings`: fontes, escala, color-scheme.
- `hyprctl getoption decoration:blur:*` e `decoration:rounding` para blur/radius.
- Wallpaper do tema para fallback de vidro.

### 10.2 Saída

- `~/.config/grimoir/theme.json` (tokens acima).
- `~/.config/grimoir/personal.toml` (opacidades padrão, preferências).
- Opcional: wallpaper desfocado em `~/.cache/grimoir/wallpaper-blur.webp`.

### 10.3 Atualização

- Binário `grimoir-personal --once` gera os arquivos.
- `grimoir-personal --watch` observa o tema do Hyprland e regenera.
- Alternativa: systemd user path unit `grimoir-theme.path` + service `grimoir-theme.service`.
- O app observa `~/.config/grimoir/theme.json`; não precisa reiniciar.

### 10.4 Política de accent

- Preferir o accent do `theme.json` do Hyprland (`active_border`/`color5`), que é o que o desktop mostra.
- gsettings `accent-color` fica como fallback, pois hoje diverge (laranja vs `#b7c9b9`).

## 11. Anotações, busca e Obsidian

### 11.1 Anotações

- PDF com texto: seleção via PDFium; highlights com quads normalizados; nota opcional.
- PDF escaneado: ferramenta de região; retângulo + nota + cor.
- EPUB: seleção JS + CFI.
- Painel lateral com lista, filtro por cor, salto para a página.
- Cores padrão: amarelo, verde, azul, rosa, roxo (configuráveis).

### 11.2 Busca no livro

- PDF: `FPDFText_FindStart`/`FindNext`; resultados com página e snippet.
- EPUB: busca via foliate-js/JS.
- Scans sem texto: aviso "sem camada de texto" + sugestão de OCR futuro.
- Navegação próximo/anterior; destaque temporário.

### 11.3 Exportação Obsidian

Dois fluxos, ambos **somente escrita no vault** (os arquivos do livro nunca são alterados):

**A. Texto selecionado → nota existente ou nova**

1. O usuário seleciona texto (PDF com camada de texto ou EPUB) e escolhe "Send to Obsidian".
2. Na primeira vez para aquele livro, abre um diálogo com:
   - campo de busca de notas do vault;
   - lista de resultados;
   - botão "Create new note".
3. Se escolher uma nota existente, o texto selecionado é **sempre anexado ao final** da nota.
4. Se criar uma nota nova, o nome padrão é o título do livro; a nota é criada e o texto é anexado.
5. O destino escolhido é cacheado em `obsidian_targets` por `book_id`.
6. Nas próximas seleções do mesmo livro, "Send to Obsidian" anexa diretamente ao destino cacheado, sem diálogo.
7. O usuário pode trocar o destino ou limpar o cache nas configurações/ações do livro.

**B. Página inteira → imagem no vault**

- Para páginas de mangá sem camada de texto, "Send page to Obsidian" renderiza a página em imagem (PNG por padrão; formato lossless/qualidade configurável) e anexa a imagem à nota de destino, com link/embed no Markdown.
- A imagem é salva na pasta de anexos configurada do vault; o Markdown referencia o caminho relativo.

**Formato das notas**

- Notas novas recebem frontmatter: título, autor, série, progresso, capa e tags.
- Anexos de texto incluem um cabeçalho com livro, página/CFI e data.
- A pasta do vault, a pasta de anexos e o template são configuráveis; o repo público não contém caminhos pessoais.
- O vault padrão é lido da configuração local do usuário; a pasta e o template exatos são definidos na camada pessoal.

## 12. Estatísticas

- Sessões: início, fim, duração, páginas.
- Agregados: tempo total, páginas, livro atual, sequência de dias.
- Pausa automática após ~2 min sem atividade.
- Tudo local; sem telemetria.
- UI simples: cartões + heatmap/lista; gráficos depois.

## 13. Personalização

- Tema: cores, fontes, raio, blur, opacidades.
- Leitor: modo, direção, fit, margens, gap entre páginas, cor de fundo.
- Biblioteca: tamanho das capas, ordenação, colunas.
- Atalhos configuráveis (fase posterior).
- CSS do usuário: arquivo `~/.config/grimoir/user.css` aplicado por último.

## 14. Performance e metas

| Métrica | Meta |
|---|---|
| Cold start | < 1,5 s |
| Scroll da biblioteca | 60 fps |
| Virada de página cacheada | < 50 ms |
| Virada de página não cacheada | < 250 ms |
| Memória com 6 páginas em cache | < 400 MB |
| Indexação de 150 PDFs (capas) | < 5 min em background |

- Cache LRU de texturas; prefetch; render fora da main thread.
- Capas geradas sob demanda, não no startup.
- Watcher com debounce para não varrer a árvore inteira.

## 15. Distribuição e publicação

- Build nativo primeiro (`cargo build --release`), com script de instalação.
- Flatpak opcional: runtime GNOME 48 tem GTK 4.18, sem `ext_background_effect`; blur via alpha + compositor.
- `.desktop` com MIME para PDF, EPUB, CBZ, TXT, MD, HTML.
- README em inglês; documentação pública em inglês. Comunicação com o autor em português.
- CI: fmt, clippy, testes, build em Ubuntu 24.04 com GTK 4.14.
- Licença: **GPL-3.0-or-later**.

## 16. Fases

### Fase 0 — Spike de risco (concluída no host)

Objetivo: validar o visual e a viabilidade antes de construir o app inteiro.

- [x] Workspace Rust mínimo + janela GTK4 transparente/fullscreen.
  - No Hyprland, `maximize()` é ignorado enquanto a janela está tiled, mesmo depois do map; o leitor entra em fullscreen e `F11` alterna.
- [x] PDFium baixado e linkado; abrir um PDF real.
- [x] Renderizar página, navegar, fit.
  - `BLAME! VOL.01.pdf`: 386 páginas, capa renderizada em 778×1100 em ~24 ms no host.
- [x] Barras laterais com alpha + blur no Hyprland.
  - Letterbox 100% transparente por padrão; barra com `backdrop-filter`.
- [x] Filtro tinta no vidro com slider.
  - Inclui detecção de páginas invertidas (fundo preto/letra branca): o papel escuro some e a letra clara permanece opaca.
- [x] Tema lido de `theme.json` e hot reload.
  - Fallback para `~/.local/state/hypr/theme.json`; watcher filtra o arquivo e evita reloads em loop.
- [x] Entregável: `cargo run -p grimoir -- <pdf>` abre o leitor com o visual desejado.

### Fase 1 — Leitor PDF (concluída no host)

- [x] Página única/dupla, RTL/LTR, zoom/pan, fullscreen.
- [x] Página dupla com animação opcional (cross-fade + deslocamento; `M` alterna).
- [x] Cache LRU, prefetch, render em thread separada e atalhos.
- [x] Barra flutuante em duas linhas com auto-hide.
- [x] Sumário (TOC) e progresso em memória.
- [x] Scroll vertical contínuo.
- [x] Tema com hot reload, filtro tinta no vidro e páginas invertidas.
- [ ] Crop automático de margens (adiado; exige detecção de conteúdo e ajuste de layout).
- [x] Persistência de progresso (implementada na Fase 2, via SQLite).
- [ ] Validação visual da animação em velocidade normal e do auto-hide com mouse real no Wayland nativo.
  - A animação foi validada em câmera lenta; o auto-hide funciona em X11 e o movimento sintético do Hyprland não gera eventos de motion para o app.

### Fase 2 — Biblioteca

- [x] SQLite com schema de livros, progresso, coleções, anotações e settings.
- [x] Varredura em background, watcher com debounce, capas PNG, metadados de PDF e detecção de séries/volumes.
- [x] Biblioteca em estilo vitrine: janela transparente, topbar/bottombar de vidro, capas 2.5D com perspectiva e sombra, grid responsivo.
- [x] Busca, filtros Continue/All/Favorites/Recent, Browse de séries/pastas, contagem e status de varredura.
- [x] “Continuar lendo” e restauração de progresso ao reabrir o livro; escrita de progresso no fechamento e nas viradas.
- [x] Abertura do leitor a partir do card e retorno à biblioteca.
- [x] Grid virtualizado (`GtkGridView`), capas carregadas sob demanda, cache de texturas e assinatura de refresh.
- [x] Capas sempre em retrato, com recorte `ContentFit::Cover` e `can_shrink`.
- [x] Worker de PDF com fila urgente/normal, render assíncrono, cache LRU de páginas e texturas, warm de spreads vizinhos.
- [x] Filtro tinta-no-vidro paralelizado com Rayon.
- [x] Watcher ignora eventos de acesso para não criar loop de varredura.
- [ ] Favoritos com UI acessível (o menu por botão direito foi removido por não funcionar).
- [ ] Coleções e edição de metadados pela UI.
- [ ] Hot reload de tema na janela da biblioteca (o leitor já tem).
- [ ] Capas/placeholder e varredura de EPUB/CBZ ficam para as fases seguintes.
- [ ] Crop automático de margens (adiado desde a Fase 1).

### Fase 3 — Anotações, busca, Obsidian, estatísticas

- Highlights de texto e região.
- Busca no livro.
- Exportação Obsidian.
- Sessões e estatísticas.

### Fase 4 — EPUB

- WebKitGTK + foliate-js, sumário, temas, seleção, CFI, progresso.
- Instalar `libwebkit2gtk-4.1-dev` no host.

### Fase 5 — Camada pessoal e polimento

- `grimoir-personal` local, watcher, systemd unit.
- Wallpaper blur fallback.
- Ajustes de tema, documentação, packaging, release.

## 17. Riscos

| Risco | Mitigação |
|---|---|
| PDFium exige binário pré-compilado | Script `fetch-pdfium.sh` + bundle no release |
| GTK 4.24 só no host; Flatpak tem 4.18 | Feature opcional; fallback alpha + blur do compositor |
| Qualidade do tinta no vidro em páginas coloridas | Auto-detecção de página cinza; toggle por livro; sliders |
| Performance em PDFs de 200 MB | Render sob demanda, cache LRU, prefetch, threads |
| WebKit dev headers ausentes | Instalar `libwebkit2gtk-4.1-dev`; módulo EPUB opcional no build |
| EPUB complexo | foliate-js; fallback de mensagem de erro |
| Indexação inicial lenta | Background, capas sob demanda, progresso |
| Licença | PDFium BSD; foliate-js MIT; escolher GPL-3.0 para o app |
| Credenciais GitHub | `gh` ausente; usar git/SSH ou token na publicação |

## 18. Perguntas abertas

- Pasta, subpasta de anexos e template exatos do vault Obsidian (o fluxo já está definido).
- Confirmar `../Grimoir-Personal/` como caminho da camada pessoal.
- Autorização para instalar `libwebkit2gtk-4.1-dev` quando a Fase 4 começar.
- Credenciais/método de publicação no GitHub (`gh` não está instalado).
- Preferências finas de tipografia e espaçamento do leitor (serão ajustadas no spike).
