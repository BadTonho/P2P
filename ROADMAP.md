# Roadmap do aplicativo P2P

Este roteiro divide o projeto em etapas para serem feitas uma de cada vez. Ao concluir uma etapa, você pode pedir: **“faça a etapa 1 do roadmap”**. Eu explico o que será feito e avanço para a próxima apenas quando você pedir.

## Escopo atual

- O desenvolvimento e a hospedagem de uma sala serão feitos no PC de quem cria a sala. Não é necessário manter um notebook separado ligado.
- Aplicativo instalado primeiro no Windows.
- Rust como linguagem principal.
- egui/eframe para a interface gráfica.
- Compartilhamento de tela como recurso principal. Chamada de voz fica como etapa opcional no final do roadmap.
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

Codificar e decodificar vídeo H.264 com OpenH264 incluído no aplicativo. Preparar quadros com até 1280×720, preservando a proporção, e limitar o envio a 30 quadros por segundo. A captura mantém o quadro mais recente; codificação e decodificação rodam fora da interface. O seletor do Windows escolhe a tela ou janela, e a prévia local permanece separada do envio.

Usar o servidor da sala apenas para trocar pedido, oferta, resposta, candidatos ICE e encerramento da sessão. Os quadros seguem diretamente entre os dois PCs; nesta etapa não há STUN ou TURN. Se a captura escolhida fechar, o usuário parar, sair da sala ou perder a conexão, encerrar a sessão WebRTC.

**Concluída quando:** o outro participante receber a tela e ela parar quando o usuário encerrar o compartilhamento ou sair da sala, sem o servidor de sinalização encaminhar imagens.

**Implementado:** captura de até 1280×720, codificação H.264 com OpenH264 incluído, envio WebRTC de até 30 fps, decodificação da tela remota, negociação por oferta/resposta e ICE, controle para pedir/aceitar/recusar/encerrar compartilhamento, desempate pela ordem de entrada quando há pedidos simultâneos, parada ao encerrar a captura ou perder a conexão. Salas com mais de duas pessoas não podem iniciar o compartilhamento.

**Aceleração de vídeo:** no Windows, o aplicativo tenta usar codificadores H.264 de hardware do Media Foundation e decodificadores DXVA/Direct3D 11. Na captura de monitores por DXGI, o app tenta redimensionar e converter BGRA para NV12 na GPU e entregar a superfície D3D11 diretamente ao encoder. A prévia lê de volta apenas o quadro reduzido; se essa etapa ou a conversão GPU falhar, usa a cópia BGRA e o caminho atual por CPU. Captura de janela continua usando Windows Graphics Capture e pode exibir a borda amarela de privacidade. Se o encoder não aceitar a superfície, o app tenta a entrada NV12 pela CPU e depois OpenH264. O decoder DXVA também pode cair para OpenH264 se não publicar quadros. Interface e logs exibem os backends e os motivos de fallback. O formato continua H.264 Baseline, até 1280×720 e teto de 30 fps.

**Pacing e diagnóstico:** o limitador aceita quadros com tolerância a pequenas variações nos callbacks, sem ultrapassar 30 FPS; quadros excedentes são descartados e uma imagem parada pode ser repetida a 1 FPS. Durante a sessão, a interface e os logs resumem a cada cinco segundos callbacks, quadros aceitos/descartados, leitura e preparação da prévia, conversão GPU, encoder, envio, recepção, decodificação e publicação.

**Validação automatizada local:** o NVIDIA H.264 Encoder MFT deste PC gerou SPS/PPS/IDR Baseline e o quadro foi decodificado pelo OpenH264, cobrindo a compatibilidade com receptores antigos. O Microsoft H264 Video Decoder MFT inicializou DXVA, mas não produziu quadros neste PC; o teste de loopback confirmou a troca para OpenH264 e a chegada da imagem.

**Ainda falta validar manualmente:** confirmar a captura DXGI sem borda em monitor principal/secundário, mudanças de resolução e fallback WGC; confirmar que o encoder Media Foundation deste PC aceita superfícies D3D11 e que a conversão DXGI→NV12 mantém o caminho GPU ativo; conferir no Gerenciador de Tarefas os mecanismos de vídeo e transmitir conteúdo em movimento entre dois computadores físicos por 30 segundos. O decoder pode ficar em CPU neste PC; o motivo aparece na interface e nos logs. Não considerar 30 FPS garantidos em hardware sem suporte.

**Validação manual:** transmissão do PC principal para o notebook confirmada, com 104 quadros decodificados e zero erros H.264; cancelamento do seletor confirmado. A transmissão inversa não foi validada porque o notebook tem hardware limitado.

**Recuperação de vídeo:** quadros RTP incompletos são descartados; após perda ou falha de decodificação, o receptor aguarda um IDR com SPS/PPS e envia PLI com limite de frequência. O emissor solicita um IDR ao codec; se o codificador de hardware não aceitar, os IDRs periódicos continuam como alternativa. Os diagnósticos agregam perdas, pedidos PLI, IDRs e o tempo até a recuperação. O teste WebRTC local cobre PLI, novo IDR e decodificação; ainda falta validar a recuperação de perda entre dois computadores físicos.

**Ainda falta validar:** fechar a janela capturada, testar o encerramento da sessão e confirmar que o servidor nunca recebe quadros de vídeo. Também falta validar com firewall e adaptadores que as pessoas realmente usarão. O teste inverso pode ser feito depois com outro computador mais potente.

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

**Versão:** a publicação inicial foi 1.0.0; a próxima compilação planejada é 1.0.9.

**Falta validar:** instalar o setup, conferir a persistência das preferências e a remoção na desinstalação. Publicar os dois assets (`p2p-client.exe` e `P2P-Voz-e-tela-Setup.exe`) em um GitHub Release e validar uma atualização futura e a recuperação após falha. Também falta validar manualmente os cenários de rede da Etapa 7 entre duas casas, incluindo a rota TURN.

**Concluída quando:** amigos conseguirem instalar pelo setup, seguir os passos para conectar-se ao PC anfitrião e compartilhar a tela, manter suas preferências ao atualizar e receber versões novas pelo atualizador opcional.

### Etapa 9 (opcional) — Fazer chamadas P2P de voz

Se a chamada de voz for desejada depois que o compartilhamento de tela estiver pronto, transmitir voz diretamente entre participantes usando WebRTC. O suporte de grupo para até oito participantes também será opcional e poderá ser feito nesta etapa.

**Concluída quando:** os participantes conseguirem falar e ouvir sem o servidor de sinalização encaminhar o áudio. Esta etapa pode ser pulada sem impedir o uso do compartilhamento de tela.

## Como pedir a próxima etapa

Use o número e o nome da etapa, por exemplo: **“Vamos fazer a etapa 1 — Preparar o projeto Rust.”** A chamada de voz está na etapa opcional final e pode ser ignorada.
