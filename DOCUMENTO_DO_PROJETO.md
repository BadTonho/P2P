# Documento do projeto

## Objetivo

Criar um aplicativo instalado no computador para fazer chamadas de voz e compartilhar a tela com amigos. O aplicativo será desenvolvido primeiro para Windows e distribuído como instalador.

## Primeira versão

O primeiro protótipo deve permitir:

- informar um nome de exibição;
- criar ou entrar em uma sala privada;
- iniciar e encerrar uma chamada de voz;
- iniciar e parar o compartilhamento da tela;
- encerrar a chamada.

O aplicativo deve pedir permissão antes de transmitir a tela e mostrar claramente quando o compartilhamento estiver ativo.

Não haverá câmera nem chat de texto na primeira versão.

## Linguagem e interface

- **Rust** será a linguagem principal do aplicativo e do servidor executado no notebook.
- **egui e eframe** serão usados para criar a interface gráfica em Rust.
- **WebRTC** será usado para tentar transmitir voz e tela diretamente entre os computadores. Uma implementação Rust a avaliar é [webrtc-rs](https://github.com/webrtc-rs/webrtc).

## Como a conexão funcionará

O objetivo é usar conexão ponto a ponto (P2P): quando a conexão direta funcionar, o áudio e a tela irão do computador de quem compartilha diretamente para os computadores dos amigos.

O notebook do dono executará um servidor de **sinalização**. Esse servidor ajuda os participantes a se encontrarem e troca as informações necessárias para iniciar a conexão. Ele não encaminha áudio nem tela quando o P2P direto funciona.

O WebRTC usa mecanismos de rede como ICE e STUN para tentar encontrar um caminho direto entre computadores que estão atrás de roteadores. A sinalização e o envio de mídia são partes diferentes da conexão.

### TURN como alternativa

Se uma rede impedir a conexão direta, um servidor TURN pode retransmitir o áudio e a tela entre os participantes. Nesse caso, os dados passam pelo TURN e a transmissão deixa de seguir o caminho direto entre os computadores.

O TURN pode ser hospedado no notebook, mas ele precisaria permanecer ligado durante a chamada e encaminharia o tráfego de voz e tela, exigindo mais capacidade de envio da conexão de internet. Ainda não foi decidido se o protótipo terá TURN como alternativa. Sem TURN, algumas redes podem não conseguir estabelecer a chamada.

## Notebook e acesso pela internet

O notebook precisa estar ligado e conectado à internet para que os amigos acessem o servidor de sinalização. Para amigos em outras casas, o servidor precisa estar acessível pela rede externa; isso pode exigir configuração do roteador. Se a conexão do provedor estiver atrás de CGNAT, conexões externas podem exigir uma solução adicional.

A primeira prova será feita entre computadores na mesma rede Wi-Fi. Depois, será testada a conexão direta entre casas diferentes.

## Etapas de desenvolvimento

1. Preparar Rust e criar o projeto para Windows.
2. Criar a janela e os controles com egui/eframe.
3. Testar uma chamada de voz entre dois computadores na mesma rede.
4. Adicionar a captura e a transmissão direta da tela.
5. Criar o servidor de sinalização em Rust no notebook.
6. Testar conexões P2P entre redes diferentes.
7. Avaliar TURN se a conexão direta falhar em algumas redes.
8. Gerar o instalador do aplicativo.

## Fora do escopo inicial

- chat de texto;
- câmera;
- feed ou funções de rede social;
- contas de usuário e histórico de mensagens;
- publicar ou hospedar um site.

## Pontos a decidir depois

- A sala será entre duas pessoas ou poderá incluir vários amigos?
- Será implementado TURN como alternativa para redes que bloqueiam P2P direto?
- Como será feito o acesso externo ao servidor no notebook caso o roteador ou o provedor bloqueie conexões de entrada?

## Referências técnicas

- [WebRTC: conexão entre participantes e sinalização](https://webrtc.org/getting-started/peer-connections)
- [WebRTC: servidor TURN](https://webrtc.org/getting-started/turn-server)
