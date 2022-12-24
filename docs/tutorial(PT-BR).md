## Tutorial
### Introdução:
Este programa é uma pequena implementação de node com um Electrum Server acoplado. Ela se comporta semelhante a um setup com Bitcoin Core + Electrum Personal Server, porém com algumas diferenças chave.  

- Node e Electrum Server estão no mesmo binário, tornando o processo mais simples e com menos erros.
- O full node utiliza uma tecnologia nova chamada `Utreexo` para reduzir o consumo de recursos, você consegue rodar o node com menos de 1GB de disco e RAM.
- Diferentemente do EPS, esse Electrum Server suporta múltiplas conexões simultâneas.

### Utilizando
Existem duas maneiras de se obter o executável. Você pode compilar do código-fonte ou baixar o binário pré compilado do Github. Para intruções de como compilar o código-fonte, veja abaixo.

### Compilando

Para compilar, você precisa da toochain do Rust e o Cargo, mais informações [aqui](https://www.rust-lang.org/).
Você pode obter o código-fonte baixando do Github ou clonando com
```bash
git clone https://www.github.com/Davidson-Souza/utreexo-electrum-server
```
Navegue para dentro da pasta com
```bash
cd utreexo-electrum-server
```

Para rodar, utilize o comando
```bash
cargo run
```
se tudo estiver ok, ele irá mostrar uma tela de ajuda com os comandos e opções do programa.

### Preparação
Antes de rodar ele pela primeira vez, você precisa extrair a xpub da sua carteira. Na Electrum, basta ir no menu "Carteira" e clicar em "Informações", a xpub vai aparecer em uma caixa de texto grande.

**Por algum motivo bizarro, eu não consegui fazer o Rust-Bitcoin aceitar zpub ou ypub, se a sua Chave Pública Extendida começar com qualquer coisa diferente de xpub ou tpub, utilize essa ferramenta -----> A FERRAMENTA AQUI <----- para converter para xpub**

Uma vez que você tenha a Chave Pública Extendida em mãos, basta inicializar o server com ela, utilizando o comando abaixo.
```bash
 cargo run --release run -- --network <network> setup <sua_xpub> <um_diretório_qualquer>
```
Onde:
- `network` é a rede que você está utilizando, bitcoin significa mainnet, outros valores válidos são signet, regtest e testnet. Todas são redes de teste que são funcionalmente idênticas a rede principal (mainnet), porém utilizada apenas para teste, e suas moedas não tem valor algum.
- `sua_xpub` é a xpub que extraímos anteriormente
- `um_diretório_qualquer` é um diretório no seu computador para guardar as informações necessárias. Não precisa de muito espaço, algo na casa de MBs. O diretório deve ser passado em valores absolutos, ex: `/home/joaozinho/.wallet/` e não `~/.wallet`. Se você quiser salvar no mesmo diretório que está o código, basta digitar o nome sem a barra inicial, ex: `minha_wallet/`.

Isso irá inicializar todos os dados que precisamos, e salvar a sua xpub para futuras inicializações. Para rodar o servidor de fato, você precisa utilizar o comando:
```bash
cargo run -- --network <network> run <um_diretório_qualquer> --rpc-user <rpc_username> --rpc-password <rpc_password> --rpc-host <rpc_host>
```
Os dois novos parâtros, `rpc_username` e `rpc_password` são os mesmos configurados no [Utreexod](https://github.com/utreexo/utreexod). Isso existe por que as mensagens que permitem o Utreexo funcionar ainda não estão implementadas no protocolo p2p do Bitcoin. Para essa demonstração simples, utilizamos o RPC do Utreexod para obter essas informações. Mas no futuro, quando as mensagens p2p estiverem finalizadas, isso não será mais necessário. Importante notar que esse código valida **todos os blocos e transações**, o Utreexod é apenas para obter informações, como se fosse um peer no seu node. Você pode baixar o seu própio Utreexod ou utilizar um público, como `XXXXXXXXXXXXXX.XXX`.