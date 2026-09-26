# Roadmap do aplicativo P2P

Este roteiro divide o projeto em etapas para serem feitas uma de cada vez. Ao concluir uma etapa, você pode pedir: **“faça a etapa 1 do roadmap”**. Eu explico o que será feito e avanço para a próxima apenas quando você pedir.

## Escopo atual

- O desenvolvimento e a hospedagem de uma sala serão feitos no PC de quem cria a sala. Não é necessário manter um notebook separado ligado.
- Aplicativo instalado primeiro no Windows.
- Rust como linguagem principal.
- egui/eframe para a interface gráfica.
- Chamada de voz e compartilhamento de tela.
- Prioridade de mídia: implementar a transmissão de tela antes da chamada de voz.
- Salas de sinalização e fila de sucessão para até oito participantes. Voz e tela em grupo serão implementadas nas etapas WebRTC posteriores.
- Sem câmera e sem chat de texto.
- Conexão P2P: áudio e tela devem ir diretamente entre os computadores quando a rede permitir.
- O aplicativo inicia um servidor de sinalização integrado no PC do anfitrião, na porta 9000. O executável separado do servidor permanece disponível para desenvolvimento.
- A sala mantém uma malha direta de controle na porta TCP 9001. Se o anfitrião sair ou ficar instável, o aplicativo elege automaticamente um sucessor elegível e reconecta os participantes ao mesmo código.
- A fila considera a pior conexão de cada candidato: perda de pulsos, jitter e latência; a ordem de entrada desempata. O anfitrião afastado por instabilidade pode voltar à fila após 30 segundos com perda abaixo de 5%.
- Radmin VPN e o ingresso dos participantes na mesma rede virtual são configurados fora do aplicativo.
- TURN continua opcional. Se for usado como alternativa, retransmite a mídia e deixa de ser uma conexão direta.

## Etapas

### Etapa 0 — Fechar o escopo da primeira versão ✅ Concluída

O protótipo atual admite até oito participantes na sala de sinalização. A chamada de voz e o compartilhamento de tela em grupo ficam para as etapas WebRTC; não haverá câmera ou chat de texto. A mídia continuará priorizando conexão P2P direta.

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

Enviar a captura da tela diretamente aos participantes usando WebRTC e permitir iniciar e parar o compartilhamento; o grupo planejado comporta até oito pessoas. A primeira validação será entre duas pessoas na mesma rede local.

**Concluída quando:** o outro participante receber a tela e ela parar quando o usuário encerrar o compartilhamento ou sair da sala, sem o servidor de sinalização encaminhar imagens.

### Etapa 6 — Fazer a chamada P2P de voz na rede local

Transmitir voz diretamente entre participantes na mesma rede local usando WebRTC, com suporte de grupo planejado para até oito participantes.

**Concluída quando:** os participantes da sala conseguirem falar e ouvir, e o servidor de sinalização não estiver encaminhando o áudio.

### Etapa 7 — Conectar participantes em casas diferentes

Deixar o servidor integrado do anfitrião acessível pela internet e testar o estabelecimento de conexões diretas com ICE/STUN. Verificar as configurações do roteador e se o provedor permite conexões de entrada.

**Concluída quando:** participantes em redes diferentes conseguirem estabelecer voz e tela diretamente, sem o servidor de sinalização retransmitir mídia.

### Etapa 8 — Decidir o tratamento de redes que bloqueiam P2P

Se a conexão direta falhar em algumas redes, decidir entre manter o requisito estrito de P2P — e informar que a chamada não pode ser estabelecida — ou adicionar TURN como alternativa. TURN retransmite voz e tela e exige mais banda no servidor.

**Concluída quando:** a decisão sobre TURN estiver tomada e o aplicativo informar claramente quando a conexão é direta ou retransmitida.

### Etapa 9 — Preparar o uso e o instalador

Tratar encerramento de chamadas, desconexões, acesso às salas e estados de captura. Gerar o instalador do aplicativo cliente e documentar o compartilhamento do endereço de rede e do código da sala.

**Concluída quando:** for possível instalar o cliente em outro computador e seguir os passos para conectar-se ao PC anfitrião.

## Como pedir a próxima etapa

Use o número e o nome da etapa, por exemplo: **“Vamos fazer a etapa 1 — Preparar o projeto Rust.”**
