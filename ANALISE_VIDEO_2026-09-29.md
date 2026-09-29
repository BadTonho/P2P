# Análise da transmissão de vídeo — 29/09/2026

## Arquivos analisados

- `p2p-2026-09-29-150353-643602400-19020.log` — PC principal.
- `p2p-2026-09-29-150344-759686600-1388.log` — notebook.

Ambos registram o aplicativo na versão **1.0.16**.

## Resultado da sessão

A transmissão funcionou nos dois sentidos. Os dois computadores usaram endereços da mesma rede física:

- PC principal: `192.168.1.133:9002`.
- Notebook: `192.168.1.201:9002`.

As sessões ICE chegaram a `Connected`, com RTT observado entre aproximadamente **3,5 e 5 ms**. Os metadados disponíveis não identificaram o par ICE selecionado, então a rota exata aparece como desconhecida; ainda assim, a conexão e o tráfego mostram que a mídia passou entre os PCs.

Isso contrasta com a tentativa anterior, em que um lado anunciava Ethernet (`192.168.1.x`) e o outro Radmin (`26.x`), e nenhuma sessão chegava a `Connected`. A escolha de interfaces compatíveis resolveu a falha de conexão daquela tentativa.

## PC principal → notebook

- Captura DXGI do monitor em `1920×1080`, reduzida para `1280×720`.
- Encoder H.264 de hardware NVIDIA ativado.
- O remetente registrou **320 quadros enviados** até o resumo das 15:04:39; o notebook registrou **367 quadros decodificados** até 15:04:42. Como são contadores amostrados em momentos diferentes, servem como aproximação, não como correspondência exata.
- A taxa observada ficou aproximadamente na faixa de **16–18 FPS**.
- O DXVA do notebook recebeu o vídeo, mas não publicou uma imagem em 750 ms. O watchdog trocou para OpenH264 na CPU; depois disso, a decodificação prosseguiu sem erros H.264.

## Notebook → PC principal

- Captura DXGI em `1366×768`, reduzida para `1280×720`.
- Encoder H.264 de hardware ativado.
- O notebook registrou **278 quadros enviados** até 15:05:12; o PC principal registrou **231 quadros decodificados** até 15:05:09. Os momentos dos resumos diferem, portanto os totais não são diretamente comparáveis.
- A taxa observada ficou aproximadamente na faixa de **14–18 FPS**.
- O DXVA do PC principal também acionou o watchdog de primeiro quadro e mudou para OpenH264. A recepção continuou sem erros de decodificação.

## Avisos e falhas

- Não há falhas de ICE, H.264 ou crash nesta sessão. O vídeo chegou e foi decodificado nos dois sentidos.
- Houve **um aviso agregado de pacote SRTP duplicado**, muito menos que os milhares observados na sessão anterior. Esse aviso isolado não demonstra duplicação de quadros pelo aplicativo.
- Os avisos `unknown TransactionID` ocorreram durante a negociação ou após a conexão e não impediram o tráfego.
- Um canal de controle foi rejeitado porque o participante ainda não constava na lista da sala; a notificação de entrada chegou cerca de 15 ms depois e a transmissão funcionou. É compatível com uma corrida na atualização da lista, sem impacto visível nesta sessão.
- Os aplicativos encerraram liberando os recursos. Não há evidência de crash.

## Conclusão e próximos passos

A incompatibilidade de adaptadores da tentativa anterior não se repetiu. O problema restante é a taxa de quadros, que ficou abaixo de 30 FPS. Estes resumos agregados não informam suficientemente a taxa de quadros aceita pela captura nem os tempos de captura e codificação; portanto, ainda não permitem atribuir o limite a uma etapa específica.

Conforme a decisão registrada, a próxima ação é seguir a **Etapa 4** de `ROADMAP_REFATORACAO.md` sem mudar o comportamento. Os **14–18 FPS** desta sessão ficam como referência. Depois da refatoração, medir captura, codificação e envio separadamente antes de otimizar o estágio limitante.
