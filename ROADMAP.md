# Roadmap do aplicativo P2P

Este roteiro divide o projeto em etapas para serem feitas uma de cada vez. Ao concluir uma etapa, você pode pedir: **“faça a etapa 1 do roadmap”**. Eu explico o que será feito e avanço para a próxima apenas quando você pedir.

## Escopo atual

- O desenvolvimento e a hospedagem de uma sala serão feitos no PC de quem cria a sala. Não é necessário manter um notebook separado ligado.
- Aplicativo instalado primeiro no Windows.
- Rust como linguagem principal.
- egui/eframe para a interface gráfica.
- Chamada de voz e compartilhamento de tela.
- Salas limitadas a duas pessoas na primeira versão: você e um amigo.
- Sem câmera e sem chat de texto.
- Conexão P2P: áudio e tela devem ir diretamente entre os computadores quando a rede permitir.
- O aplicativo inicia um servidor de sinalização integrado no PC do anfitrião, na porta 9000. O executável separado do servidor permanece disponível para desenvolvimento.
- Se o anfitrião sair normalmente, o outro participante pode aceitar e assumir a hospedagem. Se o PC anfitrião cair abruptamente, a sala termina.
- Radmin VPN e o ingresso dos participantes na mesma rede virtual são configurados fora do aplicativo.
- TURN continua opcional. Se for usado como alternativa, retransmite a mídia e deixa de ser uma conexão direta.

## Etapas

### Etapa 0 — Fechar o escopo da primeira versão ✅ Concluída

Limite definido: duas pessoas por sala — você e um amigo. A primeira versão terá chamada de voz e compartilhamento de tela, sem câmera ou chat de texto, priorizando conexão P2P direta.

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

### Etapa 4 — Sinalização integrada e transferência de anfitrião

Ao criar uma sala, iniciar o servidor de sinalização no PC do anfitrião. Os participantes trocam as informações usadas para negociar uma conexão WebRTC; o servidor não recebe nem encaminha áudio ou tela. Listar os IPv4 ativos para o anfitrião compartilhar o endereço local ou do Radmin VPN. Permitir que o participante assuma a hospedagem quando o anfitrião sair normalmente.

**Concluída quando:** dois aplicativos em computadores diferentes entrarem na sala, trocarem sinais e concluírem a transferência de anfitrião pela rede local ou pelo Radmin VPN.

**Implementado:** o cliente inicia o servidor integrado ao criar sala, mostra os adaptadores ativos com IPv4 e permite copiar `ws://IP:9000`. A transferência pede aceite, inicia o servidor no outro PC com o mesmo código e só então encerra o servidor anterior. Recusa, cancelamento ou timeout mantém a sala original ativa. O teste automatizado cobre porta ocupada, recusa, timeout e handoff entre servidores locais.

**Falta validar manualmente:** entrada e transferência entre dois computadores físicos usando LAN e Radmin VPN.

### Etapa 5 — Fazer a chamada P2P de voz na rede local

Conectar dois computadores na mesma rede Wi-Fi e transmitir o áudio diretamente entre eles usando WebRTC.

**Concluída quando:** ambos conseguirem falar e ouvir, e o servidor de sinalização não estiver encaminhando o áudio.

### Etapa 6 — Compartilhar a tela por P2P na rede local

Enviar a captura da tela diretamente ao outro participante e permitir parar o compartilhamento.

**Concluída quando:** o outro computador receber a tela e ela parar quando o usuário encerrar o compartilhamento ou a chamada.

### Etapa 7 — Conectar participantes em casas diferentes

Deixar o servidor integrado do anfitrião acessível pela internet e testar o estabelecimento de conexões diretas com ICE/STUN. Verificar as configurações do roteador e se o provedor permite conexões de entrada.

**Concluída quando:** dois participantes em redes diferentes conseguirem estabelecer voz e tela diretamente, sem o servidor de sinalização retransmitir mídia.

### Etapa 8 — Decidir o tratamento de redes que bloqueiam P2P

Se a conexão direta falhar em algumas redes, decidir entre manter o requisito estrito de P2P — e informar que a chamada não pode ser estabelecida — ou adicionar TURN como alternativa. TURN retransmite voz e tela e exige mais banda no servidor.

**Concluída quando:** a decisão sobre TURN estiver tomada e o aplicativo informar claramente quando a conexão é direta ou retransmitida.

### Etapa 9 — Preparar o uso e o instalador

Tratar encerramento de chamadas, desconexões, acesso às salas e estados de captura. Gerar o instalador do aplicativo cliente e documentar o compartilhamento do endereço de rede e do código da sala.

**Concluída quando:** for possível instalar o cliente em outro computador e seguir os passos para conectar-se ao PC anfitrião.

## Como pedir a próxima etapa

Use o número e o nome da etapa, por exemplo: **“Vamos fazer a etapa 1 — Preparar o projeto Rust.”**
