# Roadmap do aplicativo P2P

## Estado atual — 16/10/2026

O workspace está na versão 1.2.9, preparada para publicação por solicitação do mantenedor. O código inclui salas locais/Radmin, modo Internet de teste com TURN integrado opcional, compartilhamento de tela em grupo e atualização pelo GitHub Releases. O guia de uso está em `COMO_USAR.md`; o processo de publicação está em `ATUALIZACOES.md`.

**Qualidade de transmissão atual:** no modal de compartilhamento, cada pessoa escolhe 720p ou 1080p e 30 ou 60 FPS. Os quatro perfis são permitidos e a escolha é local, salva no computador; instalações novas e configurações antigas usam 720p30 por padrão. A captura preserva a proporção e não amplia fontes menores. O orçamento-alvo do encoder é 4 Mbps por espectador e 8 Mbps no total para 720p30; 6 e 12 Mbps para 720p60 ou 1080p30; e 8 e 16 Mbps para 1080p60. Em grupo, o total é dividido entre os espectadores e cada um continua limitado pela taxa individual do perfil. A taxa real varia conforme a imagem e tem custo adicional de rede. A adaptação pode reduzir bitrate, resolução e FPS temporariamente e recuperá-los em etapas, respeitando os limites do transmissor e de cada receptor. Se o hardware não acompanhar 60 FPS, a transmissão pode entregar menos quadros sem mudar o perfil salvo. Em grupo, o custo de codificação aumenta com o número de espectadores.

**Validação de mídia pendente:** testes automatizados cobrem perfis, dimensões, pacing e fallback por CPU, mas não reproduzem captura real nem desempenho de encoder/decoder em GPU. A validação manual no Windows ainda deve medir 1080p30 e 1080p60 em diferentes hardwares, incluindo a recepção acelerada e o fallback por CPU, além de transmissões em grupo com vários espectadores.

A hospedagem integrada de sinalização usa `ws://` e não termina TLS. O cliente aceita `wss://` quando se conecta a um endpoint TLS configurado externamente. No modo `ws://`, a sinalização e as credenciais TURN temporárias não têm criptografia de transporte. O compartilhamento em grupo cria sessões por participante; as portas UDP 9002–9009 podem limitar sessões simultâneas. Este resumo descreve o código, não substitui validação manual em redes e dispositivos reais.

As etapas abaixo preservam o histórico do planejamento e da implementação. Os estados, versões e pendências descritos em cada etapa correspondem ao momento em que o texto foi registrado; use o resumo acima e os guias atuais para o estado e as instruções vigentes.

Este roteiro divide o projeto em etapas para serem feitas uma de cada vez. Ao concluir uma etapa, você pode pedir: **“faça a etapa 1 do roadmap”**. Eu explico o que será feito e avanço para a próxima apenas quando você pedir.

## Escopo original

- O desenvolvimento e a hospedagem de uma sala serão feitos no PC de quem cria a sala. Não é necessário manter um notebook separado ligado.
- Aplicativo instalado primeiro no Windows.
- Rust como linguagem principal.
- egui/eframe para a interface gráfica.
- Compartilhamento de tela como recurso principal. O áudio reproduzido pelo computador pode acompanhar a tela; chamada de voz pelo microfone fica como etapa opcional no final do roadmap.
- Prioridade de mídia: implementar e validar a transmissão de tela antes de qualquer trabalho opcional de voz.
- Salas locais/Radmin de sinalização e fila de sucessão para até oito participantes. O modo Internet de teste controlado fica limitado a duas pessoas e termina quando o anfitrião sai; não tem sucessão. O compartilhamento em grupo e a chamada de voz ficam para etapas posteriores; voz não é requisito para usar o compartilhamento de tela.
- Sem câmera e sem chat de texto.
- Conexão P2P: a tela deve ir diretamente entre os computadores quando a rede permitir. Se a etapa opcional de voz for feita, o áudio também seguirá diretamente quando a rede permitir.
- O aplicativo inicia um servidor de sinalização integrado no PC do anfitrião, na porta 9000. O executável separado do servidor permanece disponível para desenvolvimento.
- A sala mantém uma malha direta de controle na porta TCP 9001. Se o anfitrião sair ou ficar instável, o aplicativo elege automaticamente um sucessor elegível e reconecta os participantes ao mesmo código.
- A fila considera a pior conexão de cada candidato: perda de pulsos, jitter e latência; a ordem de entrada desempata. O anfitrião afastado por instabilidade pode voltar à fila após 30 segundos com perda abaixo de 5%.
- Radmin VPN e o ingresso dos participantes na mesma rede virtual são configurados fora do aplicativo.
- TURN continua opcional. Se for usado como alternativa, retransmite a mídia e deixa de ser uma conexão direta.

## Etapas

### Etapa 0 — Fechar o escopo da primeira versão ✅ Concluída

O protótipo atual admite até oito participantes na sala de sinalização. O compartilhamento de tela é o recurso principal; chamada de voz é opcional e fica no final do roadmap. Não haverá câmera ou chat de texto. A mídia continuará priorizando conexão P2P direta.

TURN permanece como decisão futura. Se for habilitado como alternativa, retransmitirá áudio e tela quando a conexão P2P direta falhar.

**Concluída quando:** limite e recursos da primeira versão estiverem definidos. ✅

### Etapa 1 — Preparar o projeto Rust

Verificar as ferramentas deste PC, onde o projeto será desenvolvido, organizar os projetos do aplicativo e do servidor e abrir uma janela mínima com egui/eframe.

**Concluída quando:** o aplicativo compilar e abrir no Windows.

### Etapa 2 — Montar a interface

Criar a tela da sala com nome ou código, estado da conexão e controles para iniciar/encerrar a chamada e compartilhar/parar a tela. Não haverá controles de câmera ou chat.

**Concluída quando:** os controles e estados visuais funcionarem, ainda sem transmissão pela rede.

### Etapa 3 — Testar microfone e captura de tela no próprio computador

Implementar testes locais e independentes para o microfone e a tela:

- Disponibilizar Configurações na tela inicial e na sala, com categorias laterais. Áudio é a primeira categoria.
- Em Configurações > Áudio, capturar o microfone padrão do Windows, exibir um medidor e reproduzir a voz localmente na saída padrão. Usar uma fila curta em memória; não gravar nem transmitir áudio. Orientar o uso de fones para evitar eco.
- Na sala, abrir o seletor do Windows para escolher uma tela ou janela e exibir uma prévia atualizada. Manter só o quadro mais recente na memória, sem salvar imagens.
- Parar o teste do microfone ao sair de Áudio e parar a prévia da tela ao abrir Configurações. Sair da sala ou fechar o aplicativo também libera as capturas.
- Mostrar instruções para conferir as permissões de microfone nas configurações do Windows. O aplicativo não altera essas permissões.

**Estado:** implementação e compilação concluídas. O medidor foi confirmado com o microfone; falta conferir manualmente o retorno de áudio e o seletor/prévia de tela neste PC.

**Concluída quando:** o aplicativo confirmar que consegue captar áudio e imagem e encerrar a captura corretamente.

### Etapa 4 — Sinalização integrada, fila de sucessão e eleição de anfitrião

Ao criar uma sala, iniciar o servidor de sinalização no PC do anfitrião, na porta 9000. Admitir até oito participantes e mostrar a ordem de entrada, a autorização para hospedar e a fila baseada na pior conexão direta de cada candidato. Cada cliente abre também a porta TCP 9001 para controle direto; todos precisam permitir essa porta no firewall. Se o anfitrião sair, ficar instável ou cair, eleger automaticamente o próximo candidato e tentar o seguinte se a porta 9000 não abrir ou o servidor não ficar pronto em 10 segundos. Manter o código e as identidades; reconectar os participantes ao novo servidor. Uma partição de rede pode criar anfitriões duplicados temporariamente, que serão reconciliados quando a malha voltar.

**Concluída quando:** até oito participantes puderem entrar, consultar a fila, eleger e trocar o anfitrião automaticamente e manter o código da sala em LAN ou Radmin VPN.

**Implementado:** limite de oito participantes; identidade e ordem preservadas durante a troca; autorização para hospedar; lista e fila na interface; pulsos WebSocket diretos a cada segundo na porta 9001, medidos em janela móvel de 30 segundos; ordenação por perda, jitter, latência e ordem de entrada; eleição automática quando o anfitrião tem pelo menos cinco amostras com perda de 20% ou mais, ou perde cinco pulsos seguidos; queda do anfitrião detectada em cinco segundos; tentativa de 10 segundos por sucessor; o anfitrião afastado por instabilidade só volta após 30 segundos com perda abaixo de 5%; ação para encerrar a sala. O servidor de sinalização segue na porta 9000 e não encaminha mídia.

**Falta validar manualmente:** fila e eleição em dois ou mais computadores físicos usando LAN e Radmin VPN, incluindo falha abrupta, bloqueio da porta 9000 e reconciliação após partição.

### Etapa 5 — Compartilhar a tela por P2P na rede local

Compartilhar a captura da tela diretamente com o outro participante usando WebRTC. Esta primeira versão funciona somente quando há exatamente duas pessoas na sala; salas maiores mostram que o compartilhamento ainda não está disponível para grupos. Qualquer uma das duas pessoas pode iniciar a transmissão. Se ambas pedirem ao mesmo tempo, prevalece a pessoa que entrou primeiro na sala.

Codificar e decodificar vídeo H.264 com OpenH264 incluído no aplicativo. A implementação atual permite selecionar 720p ou 1080p e 30 ou 60 quadros por segundo, preserva a proporção e não amplia fontes menores que o perfil. A captura mantém o quadro mais recente; codificação e decodificação rodam fora da interface. O seletor do Windows escolhe a tela ou janela, e a prévia local permanece separada do envio.

Usar o servidor da sala apenas para trocar pedido, oferta, resposta, candidatos ICE e encerramento da sessão. Os quadros seguem diretamente entre os dois PCs; nesta etapa não há STUN ou TURN. Se a captura escolhida fechar, o usuário parar, sair da sala ou perder a conexão, encerrar a sessão WebRTC.

**Concluída quando:** o outro participante receber a tela e ela parar quando o usuário encerrar o compartilhamento ou sair da sala, sem o servidor de sinalização encaminhar imagens.

**Implementado:** captura de até 1920×1080, seleção local de 720p/1080p e 30/60 FPS, codificação H.264, envio WebRTC, decodificação da tela remota, negociação por oferta/resposta e ICE e controles para pedir, aceitar, recusar e encerrar compartilhamentos. O compartilhamento em grupo permite transmissões e espectadores por participante quando todos os clientes anunciam suporte; a sala aceita de duas a oito pessoas.

**Aceleração de vídeo:** no Windows, o aplicativo tenta usar codificadores H.264 de hardware do Media Foundation e decodificadores DXVA/Direct3D 11. Na captura de monitores por DXGI, o app tenta redimensionar e converter BGRA para NV12 na GPU e entregar a superfície D3D11 diretamente ao encoder. A prévia lê de volta apenas o quadro reduzido; se essa etapa ou a conversão GPU falhar, usa a cópia BGRA e o caminho atual por CPU. Captura de janela continua usando Windows Graphics Capture e pode exibir a borda amarela de privacidade. Se o encoder não aceitar a superfície, o app tenta a entrada NV12 pela CPU e depois OpenH264. O decoder DXVA também pode cair para OpenH264 se não publicar quadros. Interface e logs exibem os backends e os motivos de fallback. O fluxo segue em H.264 Baseline; os limites atuais são 1920×1080 e 60 FPS, conforme o perfil selecionado.

**Busca manual e estabilidade de brilho (desenvolvimento, versão 1.2.6 mantida):** botão **Buscar novamente** no modo local/Radmin, consulta enviada pelo worker, agregação de solicitações, manutenção da validade original do cache e reinício após falha de inicialização ou encerramento do worker. A ação mantém as preferências e não entra automaticamente em salas. Captura DXGI/WGC e desenho de vídeo opacos; parâmetros D3D11 RGB completo→NV12 BT.601 limitado definidos explicitamente para manter o contrato CPU/GPU. Os testes reproduzem o escurecimento causado por alpha antes da correção e verificam o fundo fora do cursor. A validação nativa compara cores CPU/GPU e pixels de RGBA/NV12 ao pressionar, segurar e soltar o mouse em grade, ampliação e tela cheia. Ainda falta reproduzir e aceitar o sintoma original nos dois computadores físicos com o executável atualizado.

**Correção NV12 e apresentação remota (desenvolvimento, versão 1.2.6 mantida):** a leitura do DXVA distingue dimensões visíveis, altura alocada, espaçamento por linha e início do plano UV; o receptor publica NV12 normalizado e imutável. O OpenGL/Glow apresenta as cores por shader na grade, na visualização ampliada e na tela cheia, com alternativa RGBA mantida até o encerramento da sessão se houver falha gráfica. Os recursos são reutilizados por resolução e liberados no contexto gráfico. A cópia DXVA→CPU permanece, portanto esse caminho não é zero-copy. O layout e o backend de apresentação são registrados quando mudam; os tempos de envio das texturas e desenho do shader são separados da conversão CPU. O teste de regressão cobre 1920×1080 em superfície com 1088 linhas e pitch 2048, além de recorte, buffer incompleto e troca de resolução. A opção `--validate-nv12-gpu` verifica o shader em contexto gráfico nativo sem alterar preferências. A aceitação PC→notebook em 1080p/60 e o FPS sustentado permanecem pendentes até os testes físicos descritos em `COMO_USAR.md`.

**Pacing e diagnóstico:** o limitador aceita quadros com tolerância a pequenas variações nos callbacks, sem ultrapassar os 30 ou 60 FPS selecionados; quadros excedentes são descartados e uma imagem parada pode ser repetida a 1 FPS. Durante a sessão, a interface e os logs resumem a cada cinco segundos callbacks, quadros aceitos/descartados, leitura e preparação da prévia, conversão GPU, encoder, envio, recepção, decodificação e publicação.

**Atualização do vídeo sem interação (versão 1.2.8):** o processamento de captura, sinalização e quadros recebidos passou de `App::ui` para `App::logic`, chamado pelo eframe também quando a janela está oculta ou minimizada. Durante uma sessão de vídeo, a atualização de reserva acompanha 60 FPS, substituindo o intervalo de 100 ms; produtores de quadros continuam solicitando atualização imediata. O agendamento também considera tarefas iniciadas pelos botões depois da etapa de lógica. Os testes sintéticos reproduziram a ausência de consumo de quadros com a janela oculta e o intervalo inadequado antes da correção, sem dispositivos nem cliques. Esses testes validam o ciclo da interface, não a captura real, a rede entre dois PCs ou o FPS sustentado. O sintoma relatado de a imagem só avançar ao clicar ainda precisa ser conferido nos dois computadores com o executável corrigido.

**Áudio do sistema na tela compartilhada:** opção desligada por padrão para capturar o áudio reproduzido na saída padrão do Windows; não captura o microfone e não transforma a sala em chamada de voz. O áudio segue em faixa Opus WebRTC direta junto da tela. Erros da captura, codificação, envio, recepção, decodificação ou saída são associados à sessão e à faixa nos logs, com métricas agregadas a cada cinco segundos. Uma falha de áudio não encerra o vídeo. Implementação compilada; falta validar manualmente captura e reprodução entre computadores e em salas com várias transmissões.

**Validação automatizada local (2026-10-13):** o NVIDIA H.264 Encoder MFT deste PC gerou SPS/PPS/IDR Baseline. O teste nativo produziu um quadro DXVA de 320×240 pelo Microsoft H264 Video Decoder MFT e verificou o contrato NV12 normalizado e sua conversão de referência. A suíte do workspace passou com 340 testes, incluindo a regressão do preenchimento 1080p, fallback gráfico e recuperação RTP/NACK/PLI em loopback; um tempo limite no teste de troca de anfitrião ocorreu na primeira execução, passou isoladamente e na repetição completa, sem alteração do teste. A validação explícita do shader passou nos executáveis de debug e release, comparando cores, orientação, bordas, redimensionamento e liberação de recursos. A opção `--validate-video-brightness` também passou em ambos: comparou CPU/D3D11 com diferentes valores de alpha e verificou pixels idênticos ao pressionar, segurar e soltar o mouse nos caminhos RGBA/NV12 da apresentação real do egui. Esses resultados não estabelecem aceitação da AMD Vega 8 do notebook nem 60 FPS sustentados entre computadores físicos.

**Ainda falta validar manualmente:** confirmar a captura DXGI sem borda em monitor principal/secundário, mudanças de resolução e fallback WGC; testar os perfis 1080p30 e 1080p60 em computadores físicos, confirmar a recepção por DXVA e medir o fallback por CPU. Conferir no Gerenciador de Tarefas os mecanismos de vídeo e transmitir conteúdo em movimento entre computadores, inclusive em grupo com mais de um espectador. O decoder pode ficar em CPU e o encoder pode entregar menos quadros que o perfil selecionado; o motivo aparece na interface e nos logs.

**Validação manual histórica do perfil 720p30:** transmissão do PC principal para o notebook confirmada, com 104 quadros decodificados e zero erros H.264; cancelamento do seletor confirmado. A transmissão inversa não foi validada porque o notebook tem hardware limitado. Essa verificação não valida os perfis 1080p ou 60 FPS.

**Recuperação de vídeo:** quadros RTP incompletos são descartados; após perda ou falha de decodificação, o receptor aguarda um IDR com SPS/PPS e envia PLI com limite de frequência. O emissor solicita um IDR ao codec; se o codificador de hardware não aceitar, os IDRs periódicos continuam como alternativa. Os diagnósticos agregam perdas, pedidos PLI, IDRs e o tempo até a recuperação. O teste WebRTC local cobre PLI, novo IDR e decodificação; ainda falta validar a recuperação de perda entre dois computadores físicos.

**Ainda falta validar:** fechar a janela capturada, testar o encerramento da sessão e confirmar que o servidor nunca recebe quadros de vídeo. Também falta validar com firewall e adaptadores que as pessoas realmente usarão. O teste inverso pode ser feito depois com outro computador mais potente.

### Extensão da Etapa 5 — Compartilhamento em grupo (1.0.13)

O cliente agora anuncia compatibilidade de grupo, permite que cada pessoa publique uma tela e escolha quais participantes assistir, encaminha oferta/resposta/ICE somente ao destinatário e mostra as telas assistidas em grade com foco. O modo fica disponível em salas LAN/Radmin com 2 a 8 participantes somente quando todos anunciam suporte; participantes antigos mantêm o fluxo legado de duas pessoas. **Os orçamentos de vídeo passaram a depender do perfil selecionado:** 8, 12 ou 16 Mbps no total para o grupo, conforme a qualidade, divididos entre os espectadores e limitados também pela taxa individual do perfil. Os valores detalhados estão no resumo atual e em `COMO_USAR.md`.

**Limitações arquiteturais registradas na extensão 1.0.13:** a implementação cria sessões WebRTC separadas para envio e recepção e codifica novamente para cada espectador. Portanto, não cumpre a meta original de uma conexão bidirecional por par nem de codificar uma única vez e distribuir o mesmo fluxo; a faixa UDP 9002–9009 também pode ser insuficiente quando alguém envia e assiste a várias telas ao mesmo tempo. A referência à versão 1.0.13 é histórica; a medição atual de desempenho em grupo continua pendente, conforme o resumo no início deste documento.

### Etapa 6 — Conectar participantes em casas diferentes (teste controlado)

Adicionar um modo de sala **Internet (teste)** separado de **Rede local / Radmin**. O modo local mantém até oito participantes e a sucessão atual. O modo Internet admite somente duas pessoas, não inicia a malha TCP 9001 e encerra a sala quando o anfitrião sai ou perde a conexão.

Em Configurações > Conexão, informar manualmente um IPv4 público ou nome DDNS do anfitrião. O aplicativo monta ws://endereço:9000; não consulta automaticamente o IP público. Para receber conexões, encaminhar TCP 9000 no roteador ao PC anfitrião e liberar a porta no firewall. CGNAT sem entrada pública não é contornado.

Configurar uma única URI stun: (inicialmente stun:stun.l.google.com:19302) para os dois lados da conexão WebRTC. STUN ajuda a procurar um caminho UDP direto, mas não garante que toda combinação de NAT funcione. A mídia usa UDP 9002 e não passa pelo servidor de sinalização. Não há TURN nem retransmissão de vídeo. A tentativa P2P aguarda até 30 segundos e mostra contagens de candidatos ICE públicos via STUN para ajudar no diagnóstico.

**Aviso desta etapa:** a sinalização usa ws:// sem criptografia ou autenticação; o modo serve apenas para testes controlados com pessoas conhecidas. Antes da distribuição regular, preparar wss:// e proteção de acesso.

**Implementado:** seleção entre modo local/Radmin e teste Internet; limite de duas pessoas aplicado pelo servidor e anunciado aos clientes; endereço IPv4/DDNS manual com porta 9000 fixa e botão de cópia; URI STUN editável e validada, aplicada à conexão WebRTC de envio e recepção; sem malha ou sucessão no modo Internet; diagnóstico separado de sinalização TCP 9000 e ICE/mídia UDP 9002; aviso de segurança e ausência de TURN.

**Concluída quando:** dois PCs em redes residenciais diferentes entrarem pelo endereço público e código, e compartilharem a tela diretamente. Ainda falta validar encaminhamento TCP 9000, CGNAT, firewall UDP 9002 e estabelecimento ICE entre duas casas. Também falta confirmar manualmente que a tela não passa pelo servidor.

### Etapa 7 — TURN opcional no PC anfitrião

Ao criar uma sala Internet, o anfitrião pode deixar **Usar TURN como alternativa** ativado (padrão) ou desativá-lo. O aplicativo inicia o TURN integrado no próprio processo e o encerra ao sair da sala ou fechar o app; a distribuição continua em um único `.exe`, sem processo auxiliar separado.

O anfitrião precisa encaminhar UDP 3478 e UDP 50000–50100 no roteador ao próprio PC e liberar as portas no firewall. O modo exige IPv4 público alcançável ou DDNS que resolva para ele; CGNAT não é contornado. STUN e candidatos diretos continuam habilitados, e ICE seleciona TURN apenas se o caminho direto não funcionar. O servidor de sinalização não recebe nem retransmite quadros.

O aplicativo gera credenciais temporárias para cada sala e as envia ao outro participante pela sinalização existente. Como a sinalização ainda usa `ws://` sem criptografia, as credenciais podem ser interceptadas; usar somente em testes controlados com pessoas conhecidas. `wss://` e autenticação continuam necessários antes da distribuição regular.

**Implementado:** opção TURN ativada por padrão no modo Internet; TURN/UDP integrado em UDP 3478 com faixa 50000–50100; credenciais temporárias por sala; configuração ICE com STUN e TURN; diagnóstico de falha de inicialização, erros ICE/autenticação e contagens de candidatos; identificação da rota WebRTC selecionada como direta ou retransmitida. A mídia continua fora do servidor de sinalização.

**Falta validar manualmente:** em duas casas, conferir a conexão direta quando disponível e a retransmissão TURN quando o caminho direto estiver bloqueado; testar porta ocupada, autenticação inválida, portas sem encaminhamento e encerramento do serviço ao fechar a sala. A compilação e os testes locais não substituem essa validação de roteador/firewall.

**Concluída quando:** esses cenários forem exercitados entre duas redes residenciais e a rota escolhida e a chegada dos quadros decodificados forem confirmadas.

### Etapa 8 — Preparar o uso e distribuir o aplicativo

Tratar encerramento do compartilhamento, desconexões, acesso às salas e estados de captura. Distribuir o aplicativo com um instalador Windows por usuário e também manter o `.exe` solto para o atualizador do GitHub. Documentar a instalação, o compartilhamento do endereço de rede e o código da sala.

**Atualizador migrado para GitHub Releases:** em Configurações > Atualizações, o app consulta a API pública do repositório, permite baixar sob demanda e reiniciar para aplicar. O release estável precisa ter uma tag `vMAJOR.MINOR.PATCH` e o asset `p2p-client.exe`; não é necessário incluir chave de API ou manifesto. O download valida HTTPS, tamanho, SHA-256 informado pelo GitHub e formato PE; a substituição usa um aplicador auxiliar com backup e rollback. Não há assinatura digital. As instruções de publicação estão em `ATUALIZACOES.md`.

**Instalador e preferências:** o setup por usuário instala em `%LOCALAPPDATA%\Programs\P2P-Voz-e-tela`, sem exigir administrador. O aplicativo salva preferências em `%LOCALAPPDATA%\P2P-Voz-e-tela\settings.json`; atualizações preservam o arquivo e a desinstalação o remove. Códigos de sala e credenciais temporárias não são persistidos. O script `scripts/build-installer.ps1` compila o `.exe` e o instalador Inno Setup localmente.

**Versão registrada nesta revisão:** workspace 1.2.9. A publicação inicial 1.0.0 é histórica; confira `Cargo.toml` e `ATUALIZACOES.md` para preparar a próxima versão. A validação de desempenho do compartilhamento em grupo com vários espectadores continua pendente.

**Falta validar:** instalar o setup, conferir a persistência das preferências e a remoção na desinstalação. Publicar os dois assets (`p2p-client.exe` e `P2P-Voz-e-tela-Setup.exe`) em um GitHub Release e validar uma atualização futura e a recuperação após falha. Também falta validar manualmente os cenários de rede da Etapa 7 entre duas casas, incluindo a rota TURN.

**Concluída quando:** amigos conseguirem instalar pelo setup, seguir os passos para conectar-se ao PC anfitrião e compartilhar a tela, manter suas preferências ao atualizar e receber versões novas pelo atualizador opcional.

### Etapa 9 (opcional) — Fazer chamadas P2P de voz

Se a chamada de voz for desejada depois que o compartilhamento de tela estiver pronto, transmitir voz diretamente entre participantes usando WebRTC. O suporte de grupo para até oito participantes também será opcional e poderá ser feito nesta etapa.

**Concluída quando:** os participantes conseguirem falar e ouvir sem o servidor de sinalização encaminhar o áudio. Esta etapa pode ser pulada sem impedir o uso do compartilhamento de tela.

## Como pedir a próxima etapa

Use o número e o nome da etapa, por exemplo: **“Vamos fazer a etapa 1 — Preparar o projeto Rust.”** A chamada de voz está na etapa opcional final e pode ser ignorada.

### Diagnóstico H.264 e NACK — versão 1.1.5

Separar a classificação das unidades H.264 montadas (IDR com SPS/PPS, IDR sem parâmetros, delta e sem quadro) e contabilizar os destinos por etapa: bloqueio de recuperação, espera por IDR no worker, geração antiga, admissão na fila, fila cheia/encerrada e entrada ao vivo ou reprodução em cache no decoder. Os resumos periódicos e finais são isolados por sessão e SSRC.

Correlacionar NACK enviado pelo receptor e recebido pelo emissor, além de pacotes/bytes retransmitidos quando a estatística WebRTC estiver disponível. Contadores ausentes são registrados como desconhecidos; NACK representa pacotes RTCP, não sequências solicitadas.

**Validação automatizada:** testes de classificação, destinos, reinício dos intervalos, isolamento e estatísticas opcionais; loopback WebRTC com perda RTP controlada mantendo RTCP ativo.

**Validação manual pendente:** comparar os logs dos dois PCs durante compartilhamento real e conferir SSRC, NACK e retransmissões por sessão. Os testes locais não substituem a comparação em rede física.

## Preparação da versão 1.2.7 — 2026-10-13

A versão foi incrementada para preparar o executável portátil e o instalador para o GitHub Releases. A compilação não corrige os problemas identificados no teste entre computadores: há sessões sem vídeo após perdas, gargalo de leitura no transmissor e aceitação visual de brilho/faixa verde pendente. A análise e as prioridades estão em `analises/ANALISE_LOGS_2026-10-13_TESTE_NV12_BRILHO.md`. A preparação dos artefatos não equivale à publicação nem à aceitação de 60 FPS.

A versão 1.2.7 passou nos 340 testes do workspace, formatação, build do workspace e validação gráfica nativa sintética. Executável e instalador foram compilados e suas versões e hashes conferidos; publicação manual e instalação continuam pendentes. Caminhos, datas, hashes e limitações estão em `analises/PREPARACAO_RELEASE_1.2.7.md`.

## Preparação da versão 1.2.8 — 2026-10-16

A versão foi incrementada após confirmar que 1.2.7 já está publicada. O executável portátil e o instalador foram gerados com a correção do ciclo de atualização do vídeo, usando o script do projeto e Inno Setup 6.7.3. Os 350 testes do workspace, formatação, build e validação gráfica nativa sintética passaram; as versões e os hashes dos dois arquivos foram conferidos. O teste do sintoma original nos dois computadores e a instalação manual continuam pendentes. Detalhes e arquivos para o release estável `v1.2.8` estão em `analises/PREPARACAO_RELEASE_1.2.8.md`.

## Recuperação da captura após UAC — desenvolvimento após o pacote 1.2.8

A captura DXGI de monitor tentava recriar a duplicação uma única vez após perder acesso. A confirmação de administrador pode manter a área protegida ativa, e a recriação retorna acesso negado; isso encerrava o worker e a interface encerrava também o compartilhamento. O worker agora repete apenas falhas transitórias de área de trabalho a cada 250 ms, até o acesso voltar ou o usuário parar. A espera verifica cancelamento a cada 25 ms. A recriação preserva o dispositivo quando possível e usa uma nova fábrica quando a tentativa anterior consumiu a interface inválida. A sequência dos quadros e o armazenamento da captura permanecem na mesma sessão. Erros permanentes continuam sendo retornados.

Cinco testes sintéticos cobrem acesso negado repetido, troca de área de trabalho/modo/sessão, erro permanente, cancelamento e sobrevivência do worker com publicação de novos quadros no mesmo armazenamento. Três cenários falharam antes da correção. Esses testes não exibem UAC nem acessam uma área protegida real; a retomada física de monitor e o comportamento da captura de janela continuam pendentes de validação manual. A restrição da área protegida e a necessidade de recriar a duplicação são descritas na [documentação da Microsoft](https://learn.microsoft.com/pt-br/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput).

Validação desta correção: `cargo test -p p2p-client --locked --offline recovery_tests -- --nocapture` passou nos cinco testes direcionados; `cargo fmt --all -- --check`, `cargo test --workspace --locked --offline` (355 testes: 333 cliente, 2 protocolo, 20 servidor) e `cargo build --workspace --locked --offline` passaram. O executável de desenvolvimento contém a mudança; os arquivos de produção preparados anteriormente mantêm os hashes registrados no relatório 1.2.8.

## Recuperação da qualidade adaptativa — desenvolvimento após o pacote 1.2.8

O transmissor reduzia o perfil por congestionamento usando o mesmo alvo compartilhado com o receptor, mas recuperava apenas o bitrate. Se o receptor não tivesse pedido a redução, seu controlador permanecia no perfil máximo e não enviava um pedido de retorno. Além disso, a sessão consumia os contadores do receptor a cada atualização da interface, embora o controlador só avaliasse intervalos de três segundos; a avaliação recebia apenas a última amostra curta e podia perder tanto a evidência de sobrecarga quanto a recuperação.

O limite por congestionamento agora é local a cada encoder e independente do alvo solicitado pelo receptor. Após oito segundos sem congestionamento significativo, o transmissor restaura diretamente o perfil máximo e o bitrate escolhido. O receptor mantém os contadores acumulados até completar a janela de três segundos e, após dez segundos saudáveis, volta a pedir seu perfil máximo. A composição dos limites continua respeitando a qualidade escolhida pelo transmissor; pequenas retransmissões não bloqueiam a recuperação. Novas falhas podem adiar ou reverter a subida.

Sete testes novos cobrem recuperação após redução do emissor, duas horas de falhas simuladas e retorno a 1080p30, limite mantido pelo receptor, nova redução após uma tentativa de subida falhar, atualização da interface mais rápida que os quadros, preservação dos descartes entre amostras e ausência de recuperação com fluxo vazio. Três regressões falharam antes da correção. O teste existente de congestionamento preserva as expectativas de redução do bitrate e reforço de IDR, acrescentando a expectativa de recuperação do perfil.

Validação: `cargo test -p p2p-client --locked --offline adaptive_ -- --nocapture` passou nos 24 testes direcionados; `cargo test -p p2p-client --locked --offline sender_congestion_controller -- --nocapture` passou; `cargo fmt --all -- --check`, `cargo test --workspace --locked --offline` (362 testes: 340 cliente, 2 protocolo, 20 servidor) e `cargo build --workspace --locked --offline` passaram. As horas dos cenários novos são simuladas por relógio controlado, não representam uma transmissão física de longa duração. A suíte também inclui os loopbacks existentes; a recuperação sob a rede e os equipamentos dos dois usuários ainda precisa de validação manual. Os dois computadores precisam do executável atualizado para usar a correção completa. O executável de desenvolvimento foi recompilado; o instalador anterior corresponde à preparação registrada para 1.2.8.
