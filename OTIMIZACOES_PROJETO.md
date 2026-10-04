# Sugestões de otimização do projeto

## Objetivo

Reduzir trabalho repetido na interface, captura e processamento de vídeo, além de evitar interrupções desnecessárias das sessões em grupo.

Estas sugestões resultam da leitura do código. O item 1 foi implementado e verificado automaticamente; os demais continuam como propostas. Ainda não há medição de ganho de CPU ou FPS, e estas oportunidades não demonstram a causa do FPS baixo. As sugestões específicas de áudio estão em [OTIMIZACOES_AUDIO.md](OTIMIZACOES_AUDIO.md).

## Oportunidades identificadas

| Área | Situação atual e proposta | Escopo estimado |
|---|---|---|
| Prévia local | Implementado: a textura é atualizada somente para um quadro novo ou quando precisa ser recriada. Verificação visual em monitores reais pendente. | Pequeno |
| Captura e decoder DXVA | Implementado: as texturas D3D11 de staging são mantidas em cache e reutilizadas a cada quadro, sendo recriadas apenas se dimensões ou formato mudarem. | Médio |
| Caminho GPU | A captura lê NV12 de volta para a CPU mesmo com a prévia desligada. Avaliar uma leitura somente quando necessária para a prévia ou para o fallback do encoder. | Médio |
| Sessões em grupo | O reequilíbrio encerra e recria todas as sessões de envio. Preservar as conexões existentes quando entra ou sai um espectador, considerando mudanças de bitrate. | Médio |
| Codificação de vídeo | Existe um encoder por espectador. Avaliar uma codificação compartilhada para espectadores com configurações compatíveis. | Grande |
| Logs | A escrita ocorre na própria thread que registra o evento. Avaliar uma fila limitada e um worker para escrita. | Médio |

## 1. Atualizar a prévia somente com quadro novo — implementado

**Arquivo:** `crates/p2p-client/src/app.rs`, no método `refresh_screen`.

- A chave do último quadro apresentado guarda sequência, largura e altura. A conversão e a atualização da textura ocorrem quando a chave muda ou a textura precisa ser recriada.
- A textura, a chave e a falha de conversão são limpas ao ocultar a prévia, iniciar uma captura DXGI ou pelo seletor do Windows, encerrar o compartilhamento, perder a fonte ou sair da sala.
- Reabrir a prévia usa o último quadro disponível, inclusive quando há somente NV12 pela CPU. Essa conversão utiliza o auxiliar existente e não exige uma nova captura para mostrar uma imagem parada.
- Quadros sem dados utilizáveis não avançam a chave apresentada. Uma falha de conversão é mostrada no estado de captura existente e não encerra a transmissão; o mesmo quadro com falha não é tentado novamente até mudar a chave ou reativar a prévia.
- Os auxiliares internos são exercitados com egui e quadros sintéticos, sem depender de monitor físico ou renderizador GPU.

**Validações executadas em 2026-10-02, versão 1.2.2:**

- Antes da correção, o teste de regressão reproduziu **10 atualizações de textura em 10 ciclos** com o mesmo quadro. Após a correção, o mesmo teste confirmou **1 atualização em 10 ciclos**.
- Foram adicionados 11 testes para repetição de quadro, nova sequência, dimensões diferentes, volta da sequência, recriação da textura, ocultar/reativar, ausência de dados, conversão NV12, falhas e limpeza do estado nos caminhos de encerramento.
- `cargo test -p p2p-client local_preview --locked --offline -- --nocapture`: **13 testes passaram**, incluindo dois testes existentes encontrados pelo filtro.
- `cargo fmt --all -- --check`: **passou**.
- `cargo test --workspace --locked --offline`: **176 testes passaram** (163 no cliente, 2 no protocolo e 11 no servidor), sem falhas ou testes ignorados.
- `cargo build --workspace --locked --offline`: **passou**; gerou o executável de desenvolvimento em `target/debug/p2p-client.exe`. O compilador de recursos do Windows SDK foi configurado em `RC`.

**Pendente:** conferir visualmente imagem parada e em movimento, alternar a prévia e trocar os monitores reais. Os testes comprovam a eliminação de atualizações duplicadas e a restauração com dados sintéticos; não medem ganho de FPS nem validam DXGI real. Nenhum release ou instalador foi gerado nesta etapa.

## 2. Reutilizar texturas de leitura e avaliar buffers reutilizáveis — implementado

**Arquivos:** `crates/p2p-client/src/mf_video/windows_backend/gpu_nv12.rs`, `crates/p2p-client/src/mf_video/windows_backend/mod.rs`, `crates/p2p-client/src/mf_video/windows_backend/decoder.rs` e `crates/p2p-client/src/screen_capture/dxgi_backend.rs`.

- Implementada reutilização de `staging_texture: Option<ID3D11Texture2D>` no `HardwareDecoder` (DXVA) e no loop de captura DXGI via `GpuNv12Surface::readback_nv12_into`.
- As texturas de staging agora permanecem alocadas no dispositivo D3D11 e são recriadas somente se dimensões ou formato mudarem, ou em caso de perda de acesso ao dispositivo (AccessLost), eliminando de 30 a 60 alocações por segundo na GPU.
- O mapeamento (`Map`/`Unmap`) é devidamente liberado e sincronizado a cada quadro.
- Validado com 100% de aprovação na suíte de testes do workspace.

## 3. Evitar leitura GPU → CPU desnecessária

**Arquivos:** `crates/p2p-client/src/screen_capture/dxgi_backend.rs`, `crates/p2p-client/src/screen_sharing/sender.rs` e `crates/p2p-client/src/mf_video/windows_backend/gpu_nv12.rs`.

- Avaliar a necessidade de leitura conforme a prévia e o caminho efetivamente usado pelo encoder.
- Preservar a leitura quando um encoder precisar de entrada pela CPU.
- Manter a detecção de quadros novos e o fallback após rejeição de uma superfície GPU.
- Testar prévia ligada/desligada, encoder GPU, encoder CPU e transição de fallback.

O mapeamento pode precisar esperar a GPU terminar de usar o recurso. Referência: [ID3D11DeviceContext::Map — Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map).

## 4. Preservar sessões durante mudanças de espectadores

**Arquivo:** `crates/p2p-client/src/app/group_sharing.rs`, no método `rebalance_group_outbound`.

- Criar sessões apenas para novos espectadores e encerrar apenas as removidas quando o bitrate das demais não mudar.
- Avaliar ajuste de bitrate durante a sessão quando o encoder oferecer suporte; definir o tratamento para encoders sem esse suporte.
- Preservar os limites atuais de banda e a correlação por geração.
- Testar entrada e saída de espectadores, sinais atrasados e continuidade das sessões mantidas.

## 5. Compartilhar a codificação de vídeo

**Arquivos:** `crates/p2p-client/src/app/group_sharing.rs`, `crates/p2p-client/src/screen_sharing/session.rs` e `crates/p2p-client/src/screen_sharing/sender.rs`.

- Avaliar um encoder compartilhado por conjunto de configurações compatíveis.
- Distribuir as unidades H.264 codificadas para as faixas WebRTC individuais.
- Preservar SSRC, transporte, métricas e encerramento por sessão.
- Coordenar pedidos de IDR/PLI e a entrada de novos espectadores.
- Usar filas limitadas para evitar que um espectador lento bloqueie os demais.
- Testar sessões simultâneas, recuperação após perda, entrada durante a transmissão e isolamento de erros.

É uma mudança de arquitetura e deve ficar para depois das otimizações menores.

## 6. Separar a escrita de logs dos produtores

**Arquivo:** `crates/p2p-client/src/logging.rs`.

- Avaliar um worker com fila limitada para gravação dos eventos.
- Definir o comportamento quando a fila estiver cheia e como contabilizar eventos não gravados.
- Preservar mudanças de nível em execução, rotação, retenção e exportação manual.
- Garantir que os eventos pendentes sejam tratados antes da exportação e do encerramento normal.
- Testar fila cheia, falha de escrita, ativação/desativação e encerramento.

O ganho deve ser medido especialmente com logs detalhados ativados.

## Ordem recomendada

1. Prévia somente com quadro novo — implementação e verificações automatizadas concluídas; conferência visual pendente.
2. Reutilização das texturas de leitura.
3. Preservação das sessões de grupo.
4. Leitura GPU → CPU somente quando necessária.
5. Escrita de logs em worker, conforme o custo medido.
6. Codificação de vídeo compartilhada.

Implementar cada mudança separadamente, com testes e comparação antes/depois.

## Medição e validação

- Comparar CPU, memória, alocações, tempo de leitura GPU → CPU, conversão, codificação e atualização de textura.
- Comparar FPS por estágio e latência com a mesma resolução, bitrate e conteúdo em movimento.
- Incluir imagem parada, prévia ligada/desligada, um espectador e múltiplos espectadores.
- Verificar interrupções quando alguém começa ou para de assistir.
- Executar testes direcionados e depois `cargo fmt --all -- --check`, `cargo test --workspace --locked` e `cargo build --workspace --locked`, usando `--offline` quando as dependências estiverem em cache.
- Diferenciar testes sintéticos e de loopback de validação real de GPU, DXGI, rede e dispositivos.
- Registrar os resultados antes de afirmar melhoria de FPS ou resolução de um defeito.
