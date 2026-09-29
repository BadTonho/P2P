# Roadmap de refatoração

Este roteiro organiza a divisão dos arquivos grandes por responsabilidade, sem mudar o comportamento do aplicativo. As etapas são internas e não substituem as etapas de produto do `ROADMAP.md`.

## Regras para cada etapa

- Fazer uma etapa por vez e preservar o comportamento, a interface, o protocolo e as preferências existentes.
- Não alterar a versão do aplicativo nem adicionar recursos durante a refatoração.
- Manter os testes junto do módulo responsável e acrescentar testes apenas quando uma extração exigir proteção de comportamento.
- Ao fim de cada etapa, executar `cargo fmt --all -- --check`, `cargo test --workspace --offline` e `cargo build --workspace --offline`.
- Só marcar a etapa como concluída depois que a compilação e os testes passarem.

## Etapa 1 — Separar a interface hoje concentrada em `main.rs`

`crates/p2p-client/src/main.rs` tem cerca de 5.300 linhas e reúne a inicialização do app, estado global, telas e operações de rede. Mover as telas para módulos coesos, preservando `ClientUi` como estado compartilhado durante esta primeira divisão.

Divisão prevista:

- `ui/home.rs`: tela inicial, criação e entrada em salas.
- `ui/room.rs`: palco, participantes, barra de controles e detalhes da sala.
- `ui/settings.rs`: categorias e formulários de configurações.
- `ui/diagnostics.rs`: diagnósticos, exportação de logs e apresentação de métricas.
- `app.rs`: estado `ClientUi`, ciclo de atualização, inicialização e coordenação entre telas.

**Concluída quando:** `main.rs` deixa de conter a implementação das telas; início, sala, configurações e diagnóstico continuam funcionando, e a suíte offline passa.

## Etapa 2 — Separar a coordenação da sala

Depois de mover a interface, revisar os métodos de `ClientUi` ligados à vida da sala. Extrair responsabilidades apenas onde houver uma fronteira clara:

- entrada, saída, encerramento e mudança de anfitrião;
- lista de participantes, malha de controle e fila de sucessão;
- criação e gestão de conexões de compartilhamento em grupo.

Manter `signaling_client.rs` e `control_mesh.rs` como componentes de rede; esta etapa deve separar a coordenação da interface, sem duplicar esses componentes nem alterar o protocolo.

**Concluída quando:** operações da sala puderem ser localizadas e revisadas sem percorrer o código das telas, com testes de saída, desconexão, sucessão e participantes passando.

## Etapa 3 — Dividir o pipeline em `screen_sharing.rs`

`crates/p2p-client/src/screen_sharing.rs` tem cerca de 5.600 linhas. Antes de mover código, mapear os tipos e testes e manter explícita a ordem do fluxo. Separar por responsabilidade técnica:

- sessão WebRTC e controle do ciclo de compartilhamento;
- codificação e política de quadros-chave/fallback;
- recepção RTP, remontagem H.264 e recuperação por PLI;
- decodificação e publicação dos quadros;
- métricas e diagnósticos do pipeline.

Os nomes e limites finais dos módulos devem seguir as dependências reais entre os componentes; não criar módulos só para reduzir a contagem de linhas. Preservar os testes de loopback, fallback e remontagem junto aos módulos que verificam.

**Concluída quando:** envio e recepção puderem ser inspecionados separadamente, os testes determinísticos de H.264/RTP/WebRTC passarem e o fluxo entre dois PCs continuar funcionando.

## Etapa 4 — Revisar os módulos grandes restantes

Reavaliar `control_mesh.rs` (cerca de 1.800 linhas), `mf_video.rs` (cerca de 1.700 linhas), `screen_capture.rs` (cerca de 1.100 linhas) e `update.rs` (cerca de 1.000 linhas). Eles já têm responsabilidades mais específicas; dividir somente se a revisão mostrar partes independentes que ficam mais fáceis de manter em módulos separados.

Na extração, manter `control_mesh.rs` como uma máquina de estados coesa. Separar em `screen_capture/` os backends DXGI e Windows Graphics Capture; em `mf_video/windows_backend/`, encoder, decoder, processamento NV12/GPU e utilitários H.264; e em `update/`, consulta ao GitHub, download/validação e aplicação/rollback. As fachadas preservam os pontos de entrada existentes.

**Concluída quando:** cada módulo tiver um propósito claro, sem fragmentação excessiva e sem mudança funcional.

## Ordem e acompanhamento

Fazer as etapas na ordem acima. A primeira prioridade é `main.rs`; a segunda é o pipeline de vídeo. Atualizar as caixas de estado abaixo ao concluir cada etapa.

- Etapa 1: concluída — interface extraída para módulos; `cargo fmt --all -- --check`, testes offline e build offline passaram.
- Etapa 2: concluída — ciclo de vida da sala, participantes/sucessão e compartilhamento em grupo extraídos para módulos; formatação, testes offline e build offline passaram.
- Etapa 3: extração implementada em `screen_sharing/{session,sender,receiver,metrics,h264}.rs`; `cargo fmt --all -- --check`, `cargo test --workspace --offline` e `cargo build --workspace --offline` passaram. Validação manual entre dois PCs ainda pendente.
- Etapa 4: concluída — backends de captura, componentes Media Foundation e etapas do atualizador separados em módulos; `control_mesh.rs` mantido coeso. `cargo fmt --all -- --check`, `cargo test --workspace --offline` (97 testes) e `cargo build --workspace --offline` passaram.
