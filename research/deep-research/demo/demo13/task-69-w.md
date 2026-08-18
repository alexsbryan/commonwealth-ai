Communication protocol - Wikipedia 
 Jump to content 
 Main menu 
 Main menu 
 move to sidebar 
 hide 
 Navigation
 Main page Contents Current events Random article About Wikipedia Contact us 
 Contribute
 Help Learn to edit Community portal Recent changes Upload file Special pages 
 Search 
 Search 
 Appearance 
 Donate 
 Create account 
 Log in 
 Personal tools 
 Donate 
 Create account 
 Log in 
 Contents 
 move to sidebar 
 hide 
 (Top) 
 1 
 Communicating systems 
 Toggle Communicating systems subsection 
 1.1 
 History 
 1.2 
 Concept 
 2 
 Message encoding 
 Toggle Message encoding subsection 
 2.1 
 Text-based 
 2.2 
 Binary 
 3 
 Basic requirements 
 4 
 Protocol design 
 Toggle Protocol design subsection 
 4.1 
 Layering 
 4.1.1 
 Protocol layering 
 4.1.2 
 Software layering 
 4.1.3 
 Strict layering 
 4.2 
 Design patterns 
 4.3 
 Formal specification 
 5 
 Protocol development 
 Toggle Protocol development subsection 
 5.1 
 The need for protocol standards 
 5.2 
 Standards organizations 
 5.3 
 The standardization process 
 5.4 
 OSI standardization 
 6 
 Wire image 
 7 
 Ossification 
 8 
 Taxonomies 
 9 
 See also 
 10 
 Notes 
 11 
 References 
 Toggle References subsection 
 11.1 
 Bibliography 
 12 
 External links 
 Toggle the table of contents 
 Communication protocol 
 68 languages 
 Afrikaans العربية Azərbaycanca Български বাংলা Brezhoneg Bosanski Català کوردی Čeština Dansk Deutsch Ελληνικά Esperanto Español Eesti Euskara فارسی Suomi Français Galego עברית हिन्दी Hrvatski Magyar Հայերեն Bahasa Indonesia Ido Italiano 日本語 ქართული Қазақша 한국어 Кыргызча Lëtzebuergesch Latgaļu Latviešu Олык марий Македонски മലയാളം Монгол Bahasa Melayu Nederlands Norsk nynorsk Norsk bokmål Polski پښتو Português Română Русский Srpskohrvatski / српскохрватски Simple English Slovenčina Slovenščina Српски / srpski Svenska தமிழ் ไทย Türkçe Українська اردو Oʻzbekcha / ўзбекча Vèneto Tiếng Việt 吴语 閩南語 / Bân-lâm-gí 粵語 中文 
 Edit links 
 Article 
 Talk 
 English 
 Read 
 Edit 
 View history 
 Tools 
 Tools 
 move to sidebar 
 hide 
 Actions
 Read 
 Edit 
 View history 
 General
 What links here Related changes Upload file Permanent link Page information Cite this page Get shortened URL Switch to legacy parser 
 Print/export
 Download as PDF Printable version 
 In other projects
 Wikimedia Commons Wikidata item 
 Appearance 
 move to sidebar 
 hide 
 From Wikipedia, the free encyclopedia 
 System for exchanging messages between computing systems 
 A communication protocol is a system of rules that allows two or more entities of a communications system to transmit information . The protocol defines the rules, syntax , semantics , and synchronization of communication and possible error recovery methods . Protocols may be implemented by hardware , software , or a combination of both. [ 1 ] 
 Communicating systems use well-defined formats for exchanging various messages. Each message has an exact meaning intended to elicit a response from a range of possible responses predetermined for that particular situation. The specified behavior is typically independent of how it is to be implemented . Communication protocols have to be agreed upon by the parties involved. [ 2 ] To reach an agreement, a protocol may be developed into a technical standard . A programming language describes the same for computations, so there is a close analogy between protocols and programming languages: protocols are to communication what programming languages are to computations . [ 3 ] An alternate formulation states that protocols are to communication what algorithms are to computation . [ 4 ] 
 Multiple protocols often describe different aspects of a single communication. A group of protocols designed to work together is known as a protocol suite; when implemented in software, they are a protocol stack . 
 Some of the best-known communication protocols are those related to the Internet , web, and email, which are developed and published by the Internet Engineering Task Force (IETF), and World Wide Web Consortium . Many wired and wireless protocols are also well known, such as Ethernet, Bluetooth, and, of course, mobile phone standards . These are mostly handled by the IEEE (Institute of Electrical and Electronics Engineers, e.g., Ethernet). There is also the ITU-T , which handles telecommunications protocols & formats for the public switched telephone network (PSTN). As the PSTN and Internet converge , many protocols are trending towards convergence. The International Organization for Standardization (ISO) handles many other types. 
 Communicating systems [ edit ] 
 History [ edit ] 
 Further information: Protocol Wars 
 The first use of the term protocol in a modern data communication context occurs in April 1967 in a memorandum entitled A Protocol for Use in the NPL Data Communications Network. Under the direction of Donald Davies , who pioneered packet switching at the National Physical Laboratory in the United Kingdom, it was written by Roger Scantlebury and Keith Bartlett for the NPL network . They published their work the following year. [ 5 ] [ 6 ] [ 7 ] [ 8 ] 
 On the ARPANET , the starting point for host-to-host communication in 1969 was the 1822 protocol , written by Bob Kahn , which defined the transmission of messages to an IMP. [ 9 ] The Network Control Program (NCP) for the ARPANET, developed by Steve Crocker and other graduate students including Jon Postel , was first implemented in 1970. [ 10 ] The NCP interface allowed application software to connect across the ARPANET by implementing higher-level communication protocols, an early example of the protocol layering concept. [ 11 ] 
 The CYCLADES network, designed by Louis Pouzin in the early 1970s, was the first to implement the end-to-end principle , and make the hosts responsible for the reliable delivery of data on a packet-switched network, rather than this being a service of the network itself. [ 12 ] His team was the first to tackle the highly complex problem of providing user applications with a reliable virtual circuit service while using a best-effort service , an early contribution to what will be the Transmission Control Protocol (TCP). [ 13 ] [ 14 ] [ 15 ] 
 Bob Metcalfe and others at Xerox PARC outlined the idea of Ethernet and the PARC Universal Packet (PUP) for internetworking. [ 16 ] 
 Research in the early 1970s by Bob Kahn and Vint Cerf led to the formulation of the Transmission Control Program (TCP). [ 17 ] Its RFC   675 specification was written by Cerf with Yogen Dalal and Carl Sunshine in December 1974, still a monolithic design at this time. 
 The International Network Working Group agreed on a connectionless datagram standard, which was presented to the CCITT in 1975 but was not adopted by the CCITT nor by the ARPANET. [ 18 ] Separate international research, particularly the work of Rémi Després , contributed to the development of the X.25 standard, based on virtual circuits , which was adopted by the CCITT in 1976. [ 19 ] [ 20 ] Computer manufacturers developed proprietary protocols such as IBM's Systems Network Architecture (SNA), Digital Equipment Corporation's DECnet and Xerox Network Systems . [ 21 ] 
 TCP software was redesigned as a modular protocol stack, referred to as TCP/IP. This was installed on SATNET in 1982 and on the ARPANET in January 1983. The development of a complete Internet protocol suite by 1989, as outlined in RFC   1122 and RFC   1123 , laid the foundation for the growth of TCP/IP as a comprehensive protocol suite as the core component of the emerging Internet . [ 22 ] 
 International work on a reference model for communication standards led to the OSI model , published in 1984. For a period in the late 1980s and early 1990s, engineers, organizations and nations became polarized over the issue of which standard , the OSI model or the Internet protocol suite, would result in the best and most robust computer networks. [ 23 ] [ 24 ] [ 25 ] 
 Concept [ edit ] 
 The information exchanged between devices through a network or other media is governed by rules and conventions that can be set out in communication protocol specifications. The nature of communication, the actual data exchanged and any state -dependent behaviors are defined by these specifications. In digital computing systems, the rules can be expressed by algorithms and data structures . Protocols are to communication what algorithms or programming languages are to computations. [ 3 ] [ 4 ] 
 Operating systems usually contain a set of cooperating processes that manipulate shared data to communicate with each other. This communication is governed by well-understood protocols, which can be embedded in the process code itself. [ 26 ] [ 27 ] In contrast, because there is no shared memory , communicating systems have to communicate with each other using a shared transmission medium . Transmission is not necessarily reliable, and individual systems may use different hardware or operating systems. 
 To implement a networking protocol, the protocol software modules are interfaced with a framework implemented on the machine's operating system. This framework implements the networking functionality of the operating system. [ 28 ] When protocol algorithms are expressed in a portable programming language, the protocol software may be made operating system independent. The best-known frameworks are the TCP/IP model and the OSI model . 
 At the time the Internet was developed, abstraction layering had proven to be a successful design approach for both compiler and operating system design and, given the similarities between programming languages and communication protocols, the originally monolithic networking programs were decomposed into cooperating protocols. [ 29 ] This gave rise to the concept of layered protocols, which nowadays forms the basis of protocol design. [ 30 ] 
 Systems typically do not use a single protocol to handle a transmission. Instead, they use a set of cooperating protocols, sometimes called a protocol suite . [ 31 ] Some of the best-known protocol suites are TCP/IP , IPX/SPX , X.25 , AX.25 and AppleTalk . 
 The protocols can be arranged based on functionality in groups; for instance, there is a group of transport protocols . The functionalities are mapped onto the layers, each layer solving a distinct class of problems relating to, for instance: application-, transport-, internet- and network interface-functions. [ 32 ] To transmit a message, a protocol has to be selected from each layer. The selection of the next protocol is accomplished by extending the message with a protocol selector for each layer. [ 33 ] 
 Message encoding [ edit ] 
 Communication protocols define the representation of messages exchanged between communicating systems. Common approaches to message encoding use text or binary representations. [ citation needed ] 
 Text-based [ edit ] 
 A text-based protocol or plain text protocol represents its messages in human-readable format , often in plain text encoded in a machine-readable encoding such as ASCII or UTF-8 , or in structured text-based formats such as Intel hex format , XML or JSON . 
 The immediate human readability stands in contrast with binary message representations, which have inherent benefits for use in a computer environment (such as ease of mechanical parsing and improved bandwidth utilization ). 
 Network applications have various methods of encapsulating data. One method very common with Internet protocols is a text-oriented message representation that transmits requests and responses as lines of ASCII text, terminated by a newline character (and usually a carriage return character). Examples of protocols that use plain, human-readable text for their commands are FTP ( File Transfer Protocol ), SMTP ( Simple Mail Transfer Protocol ), early versions of HTTP ( Hypertext Transfer Protocol ), and the finger protocol . [ 34 ] 
 Text-based message representations are typically easier for humans to inspect and interpret, and are therefore suitable whenever human inspection of protocol contents is required, such as during debugging and during early protocol development design phases. 
 Binary [ edit ] 
 A binary protocol uses a message representation that may utilize all values of a byte , as opposed to a text-based representation, which is limited to values corresponding to characters in a character encoding such as ASCII or UTF-8 . Binary message representations are intended to be processed by machines rather than read directly by humans. Binary protocols have the advantage of terseness, which translates into speed of transmission and interpretation. [ 35 ] 
 Binary message representations have been used in modern standards such as EbXML , HTTP/2 , HTTP/3 , and EDOC . [ 36 ] An interface in UML [ 37 ] may also be considered a binary protocol. 
 Basic requirements [ edit ] 
 Getting the data across a network is only part of the problem for a protocol. The data received has to be evaluated in the context of the progress of the conversation, so a protocol must include rules describing the context. These kinds of rules are said to express the syntax of the communication. Other rules determine whether the data is meaningful for the context in which the exchange takes place. These kinds of rules are said to express the semantics of the communication. 
 Messages are sent and received on communicating systems to establish communication. Protocols should therefore specify rules governing the transmission. In general, much of the following should be addressed: [ 38 ] 
 Data formats for data exchange 
 Digital message bitstrings are exchanged. The bitstrings are divided in fields and each field carries information relevant to the protocol. Conceptually, the bitstring is divided into two parts called the header and the payload . The actual message is carried in the payload. The header area contains the fields relevant to the operation of the protocol. Bitstrings longer than the maximum transmission unit (MTU) are divided in pieces of appropriate size. [ 39 ] 
 Address formats for data exchange 
 Addresses are used to identify both the sender and the intended receiver(s). The addresses are carried in the header area of the bitstrings, allowing the receivers to determine whether the bitstrings are of interest and should be processed or should be ignored. A connection between a sender and a receiver can be identified using an address pair (sender address, receiver address) . Usually, some address values have special meanings. An all- 1 s address could be taken to mean an addressing of all stations on the network, so sending to this address would result in a broadcast on the local network. The rules describing the meanings of the address value are collectively called an addressing scheme . [ 40 ] 
 Address mapping 
 Sometimes protocols need to map addresses of one scheme on addresses of another scheme. For instance, to translate a logical IP address specified by the application to an Ethernet MAC address. This is referred to as address mapping . [ 41 ] 
 Routing 
 When systems are not directly connected, intermediary systems along the route to the intended receiver(s) need to forward messages on behalf of the sender. On the Internet, the networks are connected using routers. The interconnection of networks through routers is called internetworking . 
 Detection of transmission errors 
 Error detection is necessary on networks where data corruption is possible. In a common approach, a CRC of the data area is added to the end of packets, making it possible for the receiver to detect differences caused by corruption. The receiver rejects the packets on CRC differences and arranges somehow for retransmission. [ 42 ] 
 Acknowledgements 
 Acknowledgement of correct reception of packets is required for connection-oriented communication . Acknowledgments are sent from receivers back to their respective senders. [ 43 ] 
 Loss of information - timeouts and retries 
 Packets may be lost on the network or be delayed in transit. To cope with this, under some protocols, a sender may expect an acknowledgment of correct reception from the receiver within a certain amount of time. Thus, on timeouts , the sender may need to retransmit the information. [ a ] In case of a permanently broken link, the retransmission has no effect, so the number of retransmissions is limited. Exceeding the retry limit is considered an error. [ 44 ] 
 Direction of information flow 
 Direction needs to be addressed if transmissions can only occur in one direction at a time as on half-duplex links or from one sender at a time as on a shared medium . This is known as media access control . Arrangements have to be made to accommodate the case of collision or contention where two parties simultaneously transmit or wish to transmit. [ 45 ] 
 Sequence control 
 If long bitstrings are divided into pieces and then sent on the network individually, the pieces may get lost or delayed or, on some types of networks, take different routes to their destination. As a result, pieces may arrive out of sequence. Retransmissions can result in duplicate pieces. By marking the pieces with sequence information at the sender, the receiver can determine what was lost or duplicated, ask for necessary retransmissions and reassemble the original message. [ 46 ] 
 Flow control 
 Flow control is needed when the sender transmits faster than the receiver or intermediate network equipment can process the transmissions. Flow control can be implemented by messaging from receiver to sender. [ 47 ] 
 Queueing 
 Communicating processes or state machines employ queues (or "buffers"), usually FIFO queues, to deal with the messages in the order sent, and may sometimes have multiple queues with different prioritization. 
 Protocol design [ edit ] 
 Systems engineering principles have been applied to create a set of common network protocol design principles. The design of complex protocols often involves decomposition into simpler, cooperating protocols. Such a set of cooperating protocols is sometimes called a protocol family or a protocol suite, [ 31 ] within a conceptual framework. 
 Communicating systems operate concurrently. An important aspect of concurrent programming is the synchronization of software for receiving and transmitting messages of communication in proper sequencing. Concurrent programming has traditionally been a topic in operating systems theory texts. [ 48 ] Formal verification seems indispensable because concurrent programs are notorious for the hidden and sophisticated bugs they contain. [ 49 ] A mathematical approach to the study of concurrency and communication is referred to as communicating sequential processes (CSP). [ 50 ] Concurrency can also be modeled using finite-state machines , such as Mealy and Moore machines . Mealy and Moore machines are in use as design tools in digital electronics systems encountered in the form of hardware used in telecommunication or electronic devices in general. [ 51 ] [ better   source   needed ] 
 The literature presents numerous analogies between computer communication and programming. In analogy, a transfer mechanism of a protocol is comparable to a central processing unit (CPU). The framework introduces rules that allow the programmer to design cooperating protocols independently of one another. 
 Layering [ edit ] 
 The TCP/IP model or Internet layering scheme and its relation to some common protocols. 
 In modern protocol design, protocols are layered to form a protocol stack. Layering is a design principle that divides the protocol design task into smaller steps, each of which accomplishes a specific part, interacting with the other parts of the protocol only in a small number of well-defined ways. Layering allows the parts of a protocol to be designed and tested without a combinatorial explosion of cases, keeping each design relatively simple. 
 The communication protocols in use on the Internet are designed to function in diverse and complex settings. Internet protocols are designed for simplicity and modularity and fit into a coarse hierarchy of functional layers defined in the Internet Protocol Suite . [ 52 ] The first two cooperating protocols, the Transmission Control Protocol (TCP) and the Internet Protocol (IP) resulted from the decomposition of the original Transmission Control Program, a monolithic communication protocol, into this layered communication suite. 
 The OSI model was developed internationally based on experience with networks that predated the Internet as a reference model for general communication with much stricter rules of protocol interaction and rigorous layering. 
 Typically, application software is built upon a robust data transport layer. Underlying this transport layer is a datagram delivery and routing mechanism that is typically connectionless in the Internet. Packet relaying across networks happens over another layer that involves only network link technologies, which are often specific to certain physical layer technologies, such as Ethernet . Layering provides opportunities to exchange technologies when needed, for example, protocols are often stacked in a tunneling arrangement to accommodate the connection of dissimilar networks. For example, IP may be tunneled across an Asynchronous Transfer Mode (ATM) network. 
 Protocol layering [ edit ] 
 Figure 3. Message flows using a protocol suite. Black loops show the actual messaging loops, red loops are the effective communication between layers enabled by the lower layers. 
 Protocol layering forms the basis of protocol design. [ 30 ] It allows the decomposition of single, complex protocols into simpler, cooperating protocols. [ 52 ] The protocol layers each solve a distinct class of communication problems. Together, the layers make up a layering scheme or model. 
 Computations deal with algorithms and data; Communication involves protocols and messages; So the analog of a data flow diagram is some kind of message flow diagram. [ 4 ] To visualize protocol layering and protocol suites, a diagram of the message flows in and between two systems, A and B, is shown in figure 3. The systems, A and B, both make use of the same protocol suite. The vertical flows (and protocols) are in-system and the horizontal message flows (and protocols) are between systems. The message flows are governed by rules and data formats specified by protocols. The blue lines mark the boundaries of the (horizontal) protocol layers. 
 Software layering [ edit ] 
 Figure 5: Protocol and software layering. The software modules implementing the protocols are represented by cubes. The information flow between the modules is represented by arrows. The (top two horizontal) red arrows are virtual. The blue lines mark the layer boundaries. 
 The software supporting protocols has a layered organization, and its relationship with protocol layering is shown in figure 5. 
 To send a message on system A, the top-layer software module interacts with the module directly below it and hands over the message to be encapsulated. The lower module fills in the header data in accordance with the protocol it implements and interacts with the bottom module, which sends the message over the communications channel to the bottom module of system B. On the receiving system B, the reverse happens, so ultimately the message gets delivered in its original form to the top module of system B. [ 53 ] 
 Program translation is divided into subproblems. As a result, the translation software is layered as well, allowing the software layers to be designed independently. The same approach can be seen in the TCP/IP layering. [ 54 ] 
 The modules below the application layer are generally considered part of the operating system. Passing data between these modules is much less expensive than passing data between an application program and the transport layer. The boundary between the application layer and the transport layer is called the operating system boundary. [ 55 ] 
 Strict layering [ edit ] 
 Strictly adhering to a layered model, a practice known as strict layering, is not always the best approach to networking. [ 56 ] Strict layering can have a negative impact on the performance of an implementation. [ 57 ] 
 Although the use of protocol layering is today ubiquitous across the field of computer networking, it has been historically criticized by many researchers [ 58 ] as abstracting the protocol stack in this way may cause a higher layer to duplicate the functionality of a lower layer, a prime example being error recovery on both a per-link basis and an end-to-end basis. [ 59 ] 
 Design patterns [ edit ] 
 Commonly recurring problems in the design and implementation of communication protocols can be addressed by software design patterns . [ 60 ] [ 61 ] [ 62 ] [ 63 ] [ 64 ] 
 Formal specification [ edit ] 
 Popular formal methods of describing communication syntax are Abstract Syntax Notation One (an ISO standard) and augmented Backus–Naur form (an IETF standard). 
 Finite-state machine models are used to formally describe the possible interactions of the protocol. [ 65 ] [ 66 ] and communicating finite-state machines [ 67 ] 
 Protocol development [ edit ] 
 For communication to occur, protocols have to be selected. The rules can be expressed by algorithms and data structures. Hardware and operating system independence is enhanced by expressing the algorithms in a portable programming language. Source independence of the specification provides wider interoperability. 
 Protocol standards are commonly created by obtaining the approval or support of a standards organization , which initiates the standardization process. The members of the standards organization agree to adhere to the work result on a voluntary basis. Often, the members are in control of large market shares relevant to the protocol, and in many cases, standards are enforced by law or the government because they are thought to serve an important public interest, so getting approval can be very important for the protocol. 
 The need for protocol standards [ edit ] 
 The need for protocol standards can be shown by looking at what happened to the Binary Synchronous Communications (BSC) protocol invented by IBM . BSC is an early link-level protocol used to connect two separate nodes. It was originally not intended to be used in a multinode network, but doing so revealed several deficiencies of the protocol. In the absence of standardization, manufacturers and organizations felt free to enhance the protocol, creating incompatible versions on their networks. In some cases, this was deliberately done to discourage users from using equipment from other manufacturers. There are more than 50 variants of the original bi-sync protocol. One can assume that a standard would have prevented at least some of this from happening. [ 28 ] 
 In some cases, protocols gain market dominance without going through a standardization process. Such protocols are referred to as de facto standards . De facto standards are common in emerging markets, niche markets, or markets that are monopolized (or oligopolized ). They can hold a market in a very negative grip, especially when used to scare away competition. From a historical perspective, standardization should be seen as a measure to counteract the ill effects of de facto standards. Positive exceptions exist; a de facto standard operating system like Linux does not have this negative grip on its market because the sources are published and maintained in an open way, thus inviting competition. 
 Standards organizations [ edit ] 
 Some of the standards organizations of relevance for communication protocols are the International Organization for Standardization (ISO), the International Telecommunication Union (ITU), the Institute of Electrical and Electronics Engineers (IEEE), and the Internet Engineering Task Force (IETF). The IETF maintains the protocols in use on the Internet. The IEEE controls many software and hardware protocols in the electronics industry for commercial and consumer devices. The ITU is an umbrella organization of telecommunication engineers designing the public switched telephone network (PSTN), as well as many radio communication systems. For marine electronics , the NMEA standards are used. The World Wide Web Consortium (W3C) produces protocols and standards for Web technologies. 
 International standards organizations are supposed to be more impartial than local organizations with a national or commercial self-interest to consider. Standards organizations also do research and development for standards of the future. In practice, the standards organizations mentioned cooperate closely with each other. [ 68 ] 
 Multiple standards bodies may be involved in the development of a protocol. If they are uncoordinated, then the result may be multiple, incompatible definitions of a protocol, or multiple, incompatible interpretations of messages; important invariants in one definition (e.g., that time-to-live values are monotone decreasing to prevent stable routing loops ) may not be respected in another. [ 69 ] 
 The standardization process [ edit ] 
 In the ISO, the standardization process starts off with the commissioning of a sub-committee workgroup. The workgroup issues working drafts and discussion documents to interested parties (including other standards bodies) in order to provoke discussion and comments. This will generate a lot of questions, much discussion and usually some disagreement. These comments are taken into account, and a draft proposal is produced by the working group. After feedback, modification, and compromise, the proposal reaches the status of a draft international standard , and ultimately an international standard . International standards are reissued periodically to address deficiencies and reflect changing views on the subject. [ 70 ] 
 OSI standardization [ edit ] 
 OSI model by layer 
 7.   Application layer 
 NNTP 
 SIP 
 SSI 
 DNS 
 FTP 
 Gopher 
 HTTP ( HTTP/3 ) 
 NFS 
 NTP 
 SMPP 
 SSH 
 SMTP 
 SNMP 
 Telnet 
 DHCP 
 NETCONF 
 more... 
 6.   Presentation layer 
 MIME 
 XDR 
 ASN.1 
 ASCII 
 TLS 
 PGP 
 more... 
 5.   Session layer 
 Named pipe 
 NetBIOS 
 SAP 
 PPTP 
 RTP 
 SOCKS 
 X.225 [ 71 ] 
 more... 
 4.   Transport layer 
 TCP 
 UDP 
 SCTP 
 DCCP 
 QUIC 
 SPX 
 more... 
 3.   Network layer 
 IP 
 IPv4 
 IPv6 
 ICMP ( ICMPv6 ) 
 IPsec 
 IGMP 
 IPX 
 IS-IS 
 AppleTalk 
 X.25   PLP 
 more... 
 2.   Data link layer 
 ATM 
 ARP 
 SDLC 
 HDLC 
 CSLIP 
 SLIP 
 GFP 
 PLIP 
 IEEE 802 
 LLC 
 MAC 
 L2TP 
 Frame Relay 
 ITU-T G.hn DLL 
 PPP 
 X.25   LAPB 
 Q.922 LAPF 
 more... 
 1.   Physical layer 
 RS-232 
 RS-449 
 ITU-T V-Series 
 I.430 
 I.431 
 PDH 
 SONET/SDH 
 PON 
 OTN 
 DSL 
 IEEE 802 
 IEEE 1394 
 ITU-T G.hn PHY 
 USB 
 Bluetooth 
 X.21 
 more... 
 v t e 
 A lesson learned from ARPANET , the predecessor of the Internet, was that protocols need a framework to operate. It is therefore important to develop a general-purpose, future-proof framework suitable for structured protocols (such as layered protocols) and their standardization. This would prevent protocol standards with overlapping functionality and would allow a clear definition of the responsibilities of a protocol at the different levels (layers). [ 72 ] This gave rise to the Open Systems Interconnection model (OSI model), which is used as a framework for the design of standard protocols and services conforming to the various layer specifications. [ 73 ] 
 In the OSI model, communicating systems are assumed to be connected by an underlying physical medium providing a basic transmission mechanism. The layers above it are numbered. Each layer provides service to the layer above it using the services of the layer immediately below it. The top layer provides services to the application process. The layers communicate with each other by means of an interface, called a service access point . Corresponding layers at each system are called peer entities . To communicate, two peer entities at a given layer use a protocol specific to that layer, which is implemented by using services of the layer below. [ 74 ] For each layer, there are two types of standards: protocol standards defining how peer entities at a given layer communicate, and service standards defining how a given layer communicates with the layer above it. 
 In the OSI model, the layers and their functionality are (from highest to lowest layer): 
 The Application layer may provide the following services to the application processes: identification of the intended communication partners, establishment of the necessary authority to communicate, determination of availability and authentication of the partners, agreement on privacy mechanisms for the communication, agreement on responsibility for error recovery and procedures for ensuring data integrity , synchronization between cooperating application processes, identification of any constraints on syntax (e.g. character sets and data structures), determination of cost and acceptable quality of service, selection of the dialogue discipline, including required logon and logoff procedures. [ 75 ] 
 The presentation layer may provide the following services to the application layer: a request for the establishment of a session, data transfer, negotiation of the syntax to be used between the application layers, any necessary syntax transformations, formatting and special purpose transformations (e.g., data compression and data encryption). [ 76 ] 
 The session layer may provide the following services to the presentation layer: establishment and release of session connections, normal and expedited data exchange, a quarantine service which allows the sending presentation entity to instruct the receiving session entity not to release data to its presentation entity without permission, interaction management so presentation entities can control whose turn it is to perform certain control functions, resynchronization of a session connection, reporting of unrecoverable exceptions to the presentation entity. [ 77 ] 
 The transport layer provides reliable and transparent data transfer in a cost-effective way as required by the selected quality of service. It may support the multiplexing of several transport connections onto one network connection or split one transport connection into several network connections. [ 78 ] 
 The network layer does the setup, maintenance and release of network paths between transport peer entities. When relays are needed, routing and relay functions are provided by this layer. The quality of service is negotiated between network and transport entities at the time the connection is set up. This layer is also responsible for network congestion control. [ 79 ] 
 The data link layer does the setup, maintenance and release of data link connections. Errors occurring in the physical layer are detected and may be corrected. Errors are reported to the network layer. The exchange of data link units (including flow control) is defined by this layer. [ 80 ] 
 The physical layer describes details like the electrical characteristics of the physical connection, the transmission techniques used, and the setup, maintenance and clearing of physical connections. [ 81 ] 
 In contrast to the TCP/IP layering scheme , which assumes a connectionless network, RM/OSI assumed a connection-oriented network. [ 82 ] Connection-oriented networks are more suitable for wide area networks and connectionless networks are more suitable for local area networks. Connection-oriented communication requires some form of session and (virtual) circuits, hence the (in the TCP/IP model lacking) session layer. The constituent members of ISO were mostly concerned with wide area networks, so the development of RM/OSI concentrated on connection-oriented networks and connectionless networks were first mentioned in an addendum to RM/OSI [ 83 ] [ 84 ] and later incorporated into an update to RM/OSI. [ 85 ] 
 At the time, [ when? ] the IETF had to cope with this and the fact that the Internet needed protocols that simply were not there. [ citation needed ] As a result, the IETF developed its own standardization process based on "rough consensus and running code". [ 86 ] The standardization process is described by RFC   2026 . 
 Nowadays, the IETF has become a standards organization for the protocols in use on the Internet. RM/OSI has extended its model to include connectionless services, and because of this, both TCP and IP could be developed into international standards. [ citation needed ] 
 Wire image [ edit ] 
 Main article: Wire data 
 The wire image of a protocol is the information that a non-participant observer is able to glean from observing the protocol messages, including both information explicitly given meaning by the protocol and inferences made by the observer. [ 87 ] Unencrypted protocol metadata is one source making up the wire image, and side-channels including packet timing also contribute. [ 88 ] Different observers with different vantages may see different wire images. [ 89 ] 
The wire image is relevant to end-user privacy and the extensibility of the protocol. [ 90 ] 
 If some portion of the wire image is not cryptographically authenticated , it is subject to modification by intermediate parties (i.e., middleboxes ), which can influence protocol operation. [ 88 ] Even if authenticated, if a portion is not encrypted, it will form part of the wire image, and intermediate parties may intervene depending on its content (e.g., dropping packets with particular flags). Signals deliberately intended for intermediary consumption may be left authenticated but unencrypted. [ 91 ] 
 The wire image can be deliberately engineered, encrypting parts that intermediaries should not be able to observe and providing signals for what they should be able to. [ 92 ] If provided signals are decoupled from the protocol's operation, they may become untrustworthy. [ 93 ] Benign network management and research are affected by metadata encryption; protocol designers must balance observability for operability and research against ossification resistance and end-user privacy. [ 90 ] The IETF announced in 2014 that it had determined that large-scale surveillance of protocol operations is an attack due to the ability to infer information from the wire image about users and their behaviour, [ 94 ] and that the IETF would "work to mitigate pervasive monitoring" in its protocol designs; [ 95 ] this had not been done systematically previously. [ 95 ] The Internet Architecture Board recommended in 2023 that disclosure of information by a protocol to the network should be intentional, [ 96 ] performed with the agreement of both recipient and sender, [ 97 ] authenticated to the degree possible and necessary, [ 98 ] only acted upon to the degree of its trustworthiness, [ 99 ] and minimised and provided to a minimum number of entities. [ 100 ] [ 101 ] Engineering the wire image and controlling what signals are provided to network elements was a "developing field" in 2023, according to the IAB. [ 102 ] 
 Ossification [ edit ] 
 Main article: protocol ossification 
 Protocol ossification is the loss of flexibility, extensibility and evolvability of network protocols . This is largely due to middleboxes that are sensitive to the wire image of the protocol, and which can interrupt or interfere with messages that are valid but which the middlebox does not correctly recognize. [ 103 ] This is a violation of the end-to-end principle . [ 104 ] Secondary causes include inflexibility in endpoint implementations of protocols. [ 105 ] 
 Ossification is a major issue in Internet protocol design and deployment, as it can prevent new protocols or extensions from being deployed on the Internet, or place strictures on the design of new protocols; new protocols may have to be encapsulated in an already-deployed protocol or mimic the wire image of another protocol. [ 106 ] Because of ossification, the Transmission Control Protocol (TCP) and User Datagram Protocol (UDP) are the only practical choices for transport protocols on the Internet, [ 107 ] and TCP itself has significantly ossified, making extension or modification of the protocol difficult. [ 108 ] 
 Recommended methods of preventing ossification include encrypting protocol metadata, [ 109 ] and ensuring that extension points are exercised and wire image variability is exhibited as fully as possible; [ 110 ] remedying existing ossification requires coordination across protocol participants. [ 111 ] QUIC is the first IETF transport protocol to have been designed with deliberate anti-ossification properties. [ 87 ] 
 Taxonomies [ edit ] 
 Classification schemes for protocols usually focus on the domain of use and function. As an example of domain of use, connection-oriented protocols and connectionless protocols are used on connection-oriented networks and connectionless networks, respectively. An example of function is a tunneling protocol , which is used to encapsulate packets in a high-level protocol so that the packets can be passed across a transport system using the high-level protocol. 
 A layering scheme combines both function and domain of use. The dominant layering schemes are the ones developed by the IETF and by ISO. Despite the fact that the underlying assumptions of the layering schemes are different enough to warrant distinguishing the two, it is a common practice to compare the two by relating common protocols to the layers of the two schemes. [ 112 ] The layering scheme from the IETF is called Internet layering or TCP/IP layering . The layering scheme from ISO is called the OSI model or ISO layering . 
 In networking equipment configuration, a term-of-art distinction is often drawn: The term protocol strictly refers to the transport layer, and the term service refers to protocols utilizing a protocol for transport. In the common case of TCP and UDP, services are distinguished by port numbers. Conformance to these port numbers is voluntary, so in content inspection systems, the term service strictly refers to port numbers, and the term application is often used to refer to protocols identified through inspection signatures. 
 See also [ edit ] 
 Cryptographic protocol   – Aspect of cryptography 
 Link access procedure 
 Lists of network protocols 
 Protocol Builder   – Programming tool to build network connectivity components 
 Notes [ edit ] 
 ↑ Failure to receive an acknowledgment indicates that either the original transmission or the acknowledgment was lost. The sender has no means to distinguish these cases and therefore, to ensure all data is received, must make the conservative assumption that the original transmission was lost. 
 References [ edit ] 
 ↑ US 7529565 , Hilpisch, Robert E.; Duchscher, Rob & Seel, Mark et al., "Wireless communication protocol", published 5 May 2009, assigned to Starkey Laboratories Inc. and Oticon AS   
 ↑ Protocol , Encyclopædia Britannica , archived from the original on 12 September 2012 , retrieved 24 September 2012 
 1 2 Comer 2000, Sect. 11.2 - The Need For Multiple Protocols, p. 177, "They (protocols) are to communication what programming languages are to computation" 
 1 2 3 Comer 2000, Sect. 1.3 - Internet Services, p. 3, "Protocols are to communication what algorithms are to computation" 
 ↑ Naughton, John (24 September 2015). A Brief History of the Future . Orion. ISBN   978-1-4746-0277-8 . 
 ↑ Campbell-Kelly, Martin (July 1987). "Data Communications at the National Physical Laboratory (1965-1975)" . IEEE Annals of the History of Computing . 9 (3): 221– 247. Bibcode : 1987IAHC....9c.221C . doi : 10.1109/MAHC.1987.10023 . 
 ↑ Pelkey, James L. "6.1 The Communications Subnet: BBN 1969" . Entrepreneurial Capitalism and Innovation: A History of Computer Communications 1968–1988 . As Kahn recalls: ... Paul Baran's contributions ... I also think Paul was motivated almost entirely by voice considerations. If you look at what he wrote, he was talking about switches that were low-cost electronics. The idea of putting powerful computers in these locations hadn't quite occurred to him as being cost effective. So the idea of computer switches was missing. The whole notion of protocols didn't exist at that time. And the idea of computer-to-computer communications was really a secondary concern. 
 ↑ Kleinrock, L. (1978). "Principles and lessons in packet communications". Proceedings of the IEEE . 66 (11): 1320– 1329. Bibcode : 1978IEEEP..66.1320K . doi : 10.1109/PROC.1978.11143 . ISSN   0018-9219 . Paul Baran ... focused on the routing procedures and on the survivability of distributed communication systems in a hostile environment, but did not concentrate on the need for resource sharing in its form as we now understand it; indeed, the concept of a software switch was not present in his work. 
 ↑ Interface Message Processor: Specifications for the Interconnection of a Host and an IMP (PDF) (Report). Bolt Beranek and Newman (BBN). Report No. 1822. 
 ↑ BOOKS, HIGH DEFINITION. UGC -NET/JRF/SET PTP & Guide Teaching and Research Aptitude: UGC -NET By HD . High Definition Books. 
 ↑ "NCP – Network Control Program" . Living Internet . Archived from the original on 7 August 2022 . Retrieved 8 October 2022 . 
 ↑ Bennett, Richard (September 2009). "Designed for Change: End-to-End Arguments, Internet Innovation, and the Net Neutrality Debate" (PDF) . Information Technology and Innovation Foundation. pp.   7, 11 . Retrieved 11 September 2017 . 
 ↑ Abbate, Janet (2000). Inventing the Internet . MIT Press. pp.   124– 127. ISBN   978-0-262-51115-5 . In fact, CYCLADES, unlike ARPANET, had been explicitly designed to facilitate internetworking; it could, for instance, handle varying formats and varying levels of service 
 ↑ Kim, Byung-Keun (2005). Internationalising the Internet the Co-evolution of Influence and Technology . Edward Elgar. pp.   51– 55. ISBN   1845426754 . In addition to the NPL Network and the ARPANET, CYCLADES, an academic and research experimental network, also played an important role in the development of computer networking technologies 
 ↑ "The internet's fifth man" . The Economist . 30 November 2013. ISSN   0013-0613 . Retrieved 22 April 2020 . In the early 1970s Mr Pouzin created an innovative data network that linked locations in France, Italy and Britain. Its simplicity and efficiency pointed the way to a network that could connect not just dozens of machines, but millions of them. It captured the imagination of Dr Cerf and Dr Kahn, who included aspects of its design in the protocols that now power the internet. 
 ↑ Moschovitis 1999 , p.   78-9 
 ↑ Cerf, V.; Kahn, R. (May 1974). "A Protocol for Packet Network Intercommunication" (PDF) . IEEE Transactions on Communications . 22 (5): 637– 648. Bibcode : 1974ITCom..22..637C . doi : 10.1109/TCOM.1974.1092259 . ISSN   1558-0857 . Archived (PDF) from the original on 6 January 2017 . Retrieved 23 February 2020 . The authors wish to thank a number of colleagues for helpful comments during early discussions of international network protocols, especially R. Metcalfe, R. Scantlebury, D. Walden, and H. Zimmerman; D. Davies and L. Pouzin who constructively commented on the fragmentation and accounting issues; and S. Crocker who commented on the creation and destruction of associations. 
 ↑ McKenzie, Alexander (January 2011). "INWG and the Conception of the Internet: An Eyewitness Account". IEEE Annals of the History of Computing . 33 (1): 66– 71. Bibcode : 2011IAHC...33a..66M . doi : 10.1109/MAHC.2011.9 . ISSN   1934-1547 . 
 ↑ Schwartz, Mischa (November 2010). "X.25 Virtual Circuits - TRANSPAC IN France - Pre-Internet Data Networking [History of communications]". IEEE Communications Magazine . 48 (11): 40– 46. doi : 10.1109/MCOM.2010.5621965 . ISSN   1558-1896 . 
 ↑ Rybczynski, Tony (December 2009). "Commercialization of packet switching (1975-1985): A Canadian perspective [History of Communications]". IEEE Communications Magazine . 47 (12): 26– 31. doi : 10.1109/MCOM.2009.5350364 . ISSN   1558-1896 . 
 ↑ The "Hidden" Prehistory of European Research Networking . Trafford Publishing. p.   354. ISBN   978-1-4669-3935-6 . 
 ↑ "TCP/IP Internet Protocol" . Living Internet . Archived from the original on 1 September 2022 . Retrieved 8 October 2022 . 
 ↑ Andrew L. Russell (30 July 2013). "OSI: The Internet That Wasn't" . IEEE Spectrum . Vol.   50, no.   8. 
 ↑ Russell, Andrew L. "Rough Consensus and Running Code' and the Internet-OSI Standards War" (PDF) . IEEE Annals of the History of Computing. Archived (PDF) from the original on 17 November 2019 . Retrieved 23 February 2020 . 
 ↑ "Standards Wars" (PDF) . 2006. Archived (PDF) from the original on 24 February 2021 . Retrieved 23 February 2020 . 
 ↑ Ben-Ari 1982, chapter 2 - The concurrent programming abstraction, p. 18-19, states the same. 
 ↑ Ben-Ari 1982, Section 2.7 - Summary, p. 27, summarizes the concurrent programming abstraction. 
 1 2 Marsden 1986, Section 6.1 - Why are standards necessary?, p. 64-65, uses BSC as an example to show the need for both standard protocols and a standard framework. 
 ↑ Comer 2000, Sect. 11.2 - The Need For Multiple Protocols, p. 177, explains this by drawing analogies between computer communication and programming languages. 
 1 2 Sect. 11.10 - The Disadvantage Of Layering, p. 192, states: layering forms the basis for protocol design. 
 1 2 Comer 2000, Sect. 11.2 - The Need For Multiple Protocols, p. 177, states the same. 
 ↑ Comer 2000, Sect. 11.3 - The Conceptual Layers Of Protocol Software, p. 178, "Each layer takes responsibility for handling one part of the problem." 
 ↑ Comer 2000, Sect. 11.11 - The Basic Idea Behind Multiplexing And Demultiplexing, p. 192, states the same. 
 ↑ Kirch, Olaf (16 January 2002). "Text Based Protocols" . Archived from the original on 30 May 2010 . Retrieved 21 October 2014 . 
 ↑ Kirch, Olaf (16 January 2002). "Binary Representation Protocols" . Archived from the original on 30 May 2010 . Retrieved 4 May 2006 . 
 ↑ Kirch, Olaf (16 January 2002). "Binary Representation Protocols" . Archived from the original on 5 March 2006 . Retrieved 4 May 2006 . 
 ↑ "Welcome To UML Web Site!" . Uml.org . Archived from the original on 30 September 2019 . Retrieved 15 January 2017 . 
 ↑ Marsden 1986, Chapter 3 - Fundamental protocol concepts and problem areas, p. 26-42, explains much of the following. 
 ↑ Comer 2000, Sect. 7.7.4 - Datagram Size, Network MTU, and Fragmentation, p. 104, explains fragmentation and the effect on the header of the fragments. 
 ↑ Comer 2000, Chapter 4 - Classful Internet Addresses, p. 64-67;71. 
 ↑ Marsden 1986, Section 14.3 - Layering concepts and general definitions, p. 187, explains address mapping. 
 ↑ Marsden 1986, Section 3.2 - Detection and transmission errors, p. 27, explains the advantages of backward error correction. 
 ↑ Marsden 1986, Section 3.3 - Acknowledgement, p. 28-33, explains the advantages of positive-only acknowledgement and mentions datagram protocols as exceptions. 
 ↑ Marsden 1986, Section 3.4 - Loss of information - timeouts and retries, p. 33-34. 
 ↑ Marsden 1986, Section 3.5 - Direction of information flow, p. 34-35, explains master/slave and the negotiations to gain control. 
 ↑ Marsden 1986, Section 3.6 - Sequence control, p. 35–36, explains how packets get lost and how sequencing solves this. 
 ↑ Marsden 1986, Section 3.7 - Flow control, p. 36–38. 
 ↑ Ben-Ari 1982, in his preface, p. xiii. 
 ↑ Ben-Ari 1982, in his preface, p. xiv. 
 ↑ Hoare 1985, Chapter 4 - Communication, p. 133, deals with communication. 
 ↑ S. Srinivasan, Digital Circuits and Systems , NPTEL courses, archived from the original on 27 December 2009 
 1 2 Comer 2000, Sect. 11.2 - The Need For Multiple Protocols, p. 177, introduces the decomposition in layers. 
 ↑ Comer 2000, Sect. 11.3 - The Conceptual Layers Of Protocol Software, p. 179, the first two paragraphs describe the sending of a message through successive layers. 
 ↑ Comer 2000, Sect. 11.2 - The need for multiple protocols, p. 178, explains similarities protocol software and compiler, assembler, linker, loader. 
 ↑ Comer 2000, Sect. 11.9.1 - Operating System Boundary, p. 192, describes the operating system boundary. 
 ↑ IETF 1989, Sect 1.3.1 - Organization, p. 15, 2nd paragraph: many design choices involve creative "breaking" of strict layering. 
 ↑ Comer 2000, Sect. 11.10 - The Disadvantage Of Layering, p. 192, explains why "strict layering can be extremely inefficient" giving examples of optimizations. 
 ↑ Wakeman, I (January 1992). "Layering considered harmful". IEEE Network : 20– 24. 
 ↑ Kurose, James; Ross, Keith (2005). Computer Networking: A Top-Down Approach . Pearson. 
 ↑ Lascano, Jorge Edison; Clyde, Stephen; Raza, Ali. "Communication-protocol Design Patterns (CommDP) - COMMDP" . Archived from the original on 18 March 2017 . Retrieved 17 March 2017 . 
 ↑ Lascano, J. E.; Clyde, S. (2016). A Pattern Language for Application-level Communication Protocols . ICSEA 2016, The Eleventh International Conference on Software Engineering Advances. pp.   22– 30. 
 ↑ Daigneau, R. (2011). Service Design Patterns: Fundamental Design Solutions for SOAP/WSDL and RESTful Web Services (1   ed.). Upper Saddle River, NJ: Addison-Wesley Professional. 
 ↑ Fowler, M. (2002). Patterns of Enterprise Application Architecture (1   ed.). Boston: Addison-Wesley Professional. ISBN   0-321-12742-0 . 
 ↑ [1]F. Buschmann, K. Henney, and D. C. Schmidt, Pattern-Oriented Software Architecture Volume 4: A Pattern Language for Distributed Computing, Volume 4 edition. Chichester England; New York: Wiley, 2007. 
 ↑ Bochmann, G. (1978). "Finite state description of communication protocols". Computer Networks . 2 ( 4– 5): 361– 372. doi : 10.1016/0376-5075(78)90015-6 . 
 ↑ Comer 2000, Glossary of Internetworking Terms and Abbreviations, p. 704, term protocol. 
 ↑ Brand, Daniel; Zafiropulo, Pitro (April 1983). "On Communicating Finite-State Machines" . Journal of the ACM . 30 (2): 323– 342. doi : 10.1145/322374.322380 . 
 ↑ Marsden 1986, Section 6.3 - Advantages of standardization, p. 66-67, states the same. 
 ↑ Bryant & Morrow 2009 , p.   4. 
 ↑ Marsden 1986, Section 6.4 - Some problems with standardisation, p. 67, follows HDLC to illustrate the process. 
 ↑ "X.225   : Information technology – Open Systems Interconnection – Connection-oriented Session protocol: Protocol specification" . Archived from the original on 1 February 2021 . Retrieved 10 March 2023 . 
 ↑ Marsden 1986, Section 6.1 - Why are standards necessary?, p. 65, explains lessons learned from ARPANET. 
 ↑ Marsden 1986, Section 14.1 - Introduction, p. 181, introduces OSI. 
 ↑ Marsden 1986, Section 14.3 - Layering concepts and general definitions, p. 183-185, explains terminology. 
 ↑ Marsden 1986, Section 14.4 - The application layer, p. 188, explains this. 
 ↑ Marsden 1986, Section 14.5 - The presentation layer, p. 189, explains this. 
 ↑ Marsden 1986, Section 14.6 - The session layer, p. 190, explains this. 
 ↑ Marsden 1986, Section 14.7 - The transport layer, p. 191, explains this. 
 ↑ Marsden 1986, Section 14.8 - The network layer, p. 192, explains this. 
 ↑ Marsden 1986, Section 14.9 - The data link layer, p. 194, explains this. 
 ↑ Marsden 1986, Section 14.10 - The physical layer, p. 195, explains this. 
 ↑ ISO 7498:1984 – Information processing systems - Open Systems Interconnection - Basic Reference Model . ISO . p.   5. This Basic Reference Model of Open Systems Interconnection is based on the assumption that a connection is required for the transfer of data. 
 ↑ ISO 7498:1984/ADD 1:1987 – Information processing systems — Open Systems Interconnection — Basic Reference Model — Addendum 1 . ISO . 
 ↑ Marsden 1986, Section 14.11 - Connectionless mode and RM/OSI, p. 195, mentions this. 
 ↑ ISO 7498:1994 – Information processing systems - Open Systems Interconnection - Basic Reference Model . ISO . 
 ↑ Comer 2000, Section 1.9 - Internet Protocols And Standardization, p. 12, explains why the IETF did not use existing protocols. 
 1 2 Trammell & Kuehlewind 2019 , p.   2. 
 1 2 Trammell & Kuehlewind 2019 , p.   3. 
 ↑ Trammell & Kuehlewind 2019 , p.   4. 
 1 2 Fairhurst & Perkins 2021 , 7. Conclusions. 
 ↑ Trammell & Kuehlewind 2019 , p.   5. 
 ↑ Trammell & Kuehlewind 2019 , p.   6. 
 ↑ Trammell & Kuehlewind 2019 , p.   7-8. 
 ↑ Farrell & Tschofenig 2014 , p.   2. 
 1 2 Farrell & Tschofenig 2014 , p.   3. 
 ↑ Arkko et al. 2023 , 2.1. Intentional Distribution. 
 ↑ Arkko et al. 2023 , 2.2. Control of the Distribution of Information. 
 ↑ Arkko et al. 2023 , 2.3. Protecting Information and Authentication. 
 ↑ Arkko et al. 2023 , 2.5. Limiting Impact of Information. 
 ↑ Arkko et al. 2023 , 2.4. Minimize Information. 
 ↑ Arkko et al. 2023 , 2.6. Minimum Set of Entities. 
 ↑ Arkko et al. 2023 , 3. Further Work. 
 ↑ Papastergiou et al. 2017 , p.   619. 
 ↑ Papastergiou et al. 2017 , p.   620. 
 ↑ Papastergiou et al. 2017 , p.   620-621. 
 ↑ Papastergiou et al. 2017 , p.   623-4. 
 ↑ McQuistin, Perkins & Fayed 2016 , p.   1. 
 ↑ Thomson & Pauly 2021 , A.5. TCP. 
 ↑ Hardie 2019 , p.   7-8. 
 ↑ Thomson & Pauly 2021 , 3. Active Use. 
 ↑ Thomson & Pauly 2021 , 3.5. Restoring Active Use. 
 ↑ Comer 2000, Sect. 11.5.1 - The TCP/IP 5-Layer Reference Model, p. 183, states the same. 
 Bibliography [ edit ] 
 Radia Perlman (1999). Interconnections: Bridges, Routers, Switches, and Internetworking Protocols (2nd   ed.). Addison-Wesley. ISBN   0-201-63448-1 . . In particular Ch. 18 on "network design folklore", which is also available online 
 Gerard J. Holzmann (1991). Design and Validation of Computer Protocols . Prentice Hall. ISBN   0-13-539925-4 . 
 Douglas E. Comer (2000). Internetworking with TCP/IP - Principles, Protocols and Architecture (4th   ed.). Prentice Hall. ISBN   0-13-018380-6 . In particular Ch.11 Protocol layering. Also has a RFC guide and a Glossary of Internetworking Terms and Abbreviations. 
 R. Braden , ed. (1989). Requirements for Internet Hosts -- Communication Layers . Internet Engineering Task Force abbr. IETF. doi : 10.17487/RFC1122 . RFC 1122 . Describes TCP/IP to the implementors of protocol software. In particular, the introduction gives an overview of the design goals of the suite. 
 M. Ben-ari (1982). Principles of concurrent programming (10th Print   ed.). Prentice Hall International. ISBN   0-13-701078-8 . 
 C.A.R. Hoare (1985). Communicating sequential processes (10th Print   ed.). Prentice Hall International. ISBN   0-13-153271-5 . 
 R.D. Tennent (1981). Principles of programming languages (10th Print   ed.). Prentice Hall International. ISBN   0-13-709873-1 . 
 Brian W Marsden (1986). Communication network protocols (2nd   ed.). Chartwell Bratt. ISBN   0-86238-106-1 . 
 Andrew S. Tanenbaum (1984). Structured computer organization (10th Print   ed.). Prentice Hall International. ISBN   0-13-854605-3 . 
 Bryant, Stewart; Morrow, Monique, eds. (November 2009). Uncoordinated Protocol Development Considered Harmful . IETF . doi : 10.17487/RFC5704 . RFC 5704 . 
 Farrell, Stephen; Tschofenig, Hannes (May 2014). Pervasive Monitoring Is an Attack . IETF . doi : 10.17487/RFC7258 . RFC 7258 . 
 Trammell, Brian; Kuehlewind, Mirja (April 2019). The Wire Image of a Network Protocol . IETF . doi : 10.17487/RFC8546 . RFC 8546 . 
 Hardie, Ted, ed. (April 2019). Transport Protocol Path Signals . IETF . doi : 10.17487/RFC8558 . RFC 8558 . 
 Fairhurst, Gorry; Perkins, Colin (July 2021). Considerations around Transport Header Confidentiality, Network Operations, and the Evolution of Internet Transport Protocols . IETF . doi : 10.17487/RFC9065 . RFC 9065 . 
 Thomson, Martin; Pauly, Tommy (December 2021). Long-Term Viability of Protocol Extension Mechanisms . IETF . doi : 10.17487/RFC9170 . RFC 9170 . 
 Arkko, Jari; Hardie, Ted; Pauly, Tommy; Kühlewind, Mirja (July 2023). Considerations on Application - Network Collaboration Using Path Signals . IETF . doi : 10.17487/RFC9419 . RFC 9419 . 
 McQuistin, Stephen; Perkins, Colin; Fayed, Marwan (July 2016). Implementing Real-Time Transport Services over an Ossified Network . 2016 Applied Networking Research Workshop. doi : 10.1145/2959424.2959443 . hdl : 1893/26111 . 
 Papastergiou, Giorgos; Fairhurst, Gorry; Ros, David; Brunstrom, Anna; Grinnemo, Karl-Johan; Hurtig, Per; Khademi, Naeem; Tüxen, Michael; Welzl, Michael; Damjanovic, Dragana; Mangiante, Simone (2017). "De-Ossifying the Internet Transport Layer: A Survey and Future Perspectives". IEEE Communications Surveys & Tutorials . 19 : 619– 639. doi : 10.1109/COMST.2016.2626780 . hdl : 2164/8317 . 
 Moschovitis, Christos J. P. (1999). History of the Internet: A Chronology, 1843 to the Present . ABC-CLIO. ISBN   978-1-57607-118-2 . 
 External links [ edit ] 
 Javvin's Protocol Dictionary at the Wayback Machine (archived 2004-06-10) 
 Overview of protocols in telecontrol field with OSI Reference Model 
 v t e Telecommunications History 
 Beacon 
 Broadcasting 
 Cable protection system 
 Cable TV 
 Communications satellite 
 Computer network 
 Data compression 
 audio 
 DCT 
 image 
 video 
 Digital media 
 Internet video 
 online video platform 
 social media 
 streaming 
 Drums 
 Edholm's law 
 Electrical telegraph 
 Fax 
 Heliographs 
 Hydraulic telegraph 
 Information Age 
 Information revolution 
 Internet 
 Mass media 
 Mobile phone 
 Smartphone 
 Optical telecommunication 
 Optical telegraphy 
 Pager 
 Photophone 
 Prepaid mobile phone 
 Radio 
 Radiotelephone 
 Satellite communications 
 Semaphore 
 Phryctoria 
 Semiconductor 
 device 
 MOSFET 
 transistor 
 Smoke signals 
 Telecommunications history 
 Telautograph 
 Telegraphy 
 Teleprinter (teletype) 
 Telephone 
 history 
 The Telephone Cases 
 Television 
 digital 
 streaming 
 Undersea telegraph line 
 Videotelephony 
 Whistled language 
 Wireless revolution 
 Pioneers 
 Nasir Ahmed 
 Edwin Howard Armstrong 
 Mohamed M. Atalla 
 John Logie Baird 
 Paul Baran 
 John Bardeen 
 Alexander Graham Bell 
 Emile Berliner 
 Tim Berners-Lee 
 Francis Blake 
 Jagadish Chandra Bose 
 Charles Bourseul 
 Walter Houser Brattain 
 Vint Cerf 
 Claude Chappe 
 Yogen Dalal 
 Donald Davies 
 Daniel Davis Jr. 
 Amos Dolbear 
 Thomas Edison 
 Philo Farnsworth 
 Reginald Fessenden 
 Lee de Forest 
 Elisha Gray 
 Oliver Heaviside 
 Robert Hooke 
 Erna Schneider Hoover 
 Harold Hopkins 
 Gardiner Greene Hubbard 
 Bob Kahn 
 Dawon Kahng 
 Charles K. Kao 
 Narinder Singh Kapany 
 Hedy Lamarr 
 Roberto Landell 
 Innocenzo Manzetti 
 Guglielmo Marconi 
 Robert Metcalfe 
 Antonio Meucci 
 Samuel Morse 
 Jun-ichi Nishizawa 
 Charles Grafton Page 
 Radia Perlman 
 Alexander Stepanovich Popov 
 Tivadar Puskás 
 Johann Philipp Reis 
 Claude Shannon 
 Almon Brown Strowger 
 Henry Sutton 
 Charles Sumner Tainter 
 Nikola Tesla 
 Camille Tissot 
 Alfred Vail 
 Thomas A. Watson 
 Charles Wheatstone 
 Vladimir K. Zworykin 
 Internet pioneers 
 Transmission media 
 Coaxial cable 
 Fiber-optic communication 
 optical fiber 
 Free-space optical communication 
 Molecular communication 
 Radio waves 
 wireless 
 Transmission line 
 telecommunication circuit 
 Network topology and switching 
 Bandwidth 
 Links 
 Network switching 
 circuit 
 packet 
 Nodes 
 terminal 
 Telephone exchange 
 Multiplexing 
 Space-division 
 Frequency-division 
 Time-division 
 Polarization-division 
 Orbital angular-momentum 
 Code-division 
 Concepts 
 Communication protocol 
 Computer network 
 Data transmission 
 Store and forward 
 Telecommunications equipment 
 Types of network 
 Cellular network 
 Ethernet 
 ISDN 
 LAN 
 Mobile 
 NGN 
 Public Switched Telephone 
 Radio 
 Television 
 Telex 
 UUCP 
 WAN 
 Wireless network 
 Notable networks 
 ARPANET 
 BITNET 
 CYCLADES 
 FidoNet 
 Internet 
 Internet2 
 JANET 
 NPL network 
 TANet 
 Toasternet 
 Usenet 
 Locations 
 Africa 
 Americas
 North 
 South 
 Antarctica 
 Asia 
 Europe 
 Oceania 
 Global telecommunications regulation bodies 
 Telecommunication portal 
 Category 
 Outline 
 Commons 
 v t e Computer science This template follows roughly the 2012 ACM Computing Classification System Hardware 
 Printed circuit board 
 Peripheral 
 Integrated circuit 
 Very-large-scale integration 
 System on a chip (SoC) 
 Energy consumption (green computing) 
 Electronic design automation 
 Hardware acceleration 
 Processor 
 Size – Form 
 Systems organization 
 Computer architecture 
 Computational complexity 
 Dependability 
 Embedded system 
 Real-time computing 
 Cyber-physical system 
 Fault tolerance 
 Wireless sensor network 
 Networks 
 Network architecture 
 Network protocol 
 Network components 
 Network scheduler 
 Network performance evaluation 
 Network service 
 Software organization 
 Interpreter 
 Middleware 
 Virtual machine 
 Operating system 
 Software quality 
 Software notations , tools 
 Programming paradigm 
 Programming language 
 Compiler 
 Domain-specific language 
 Modeling language 
 Software framework 
 Integrated development environment 
 Software configuration management 
 Software library 
 Software repository 
 Software development 
 Control flow 
 Software development process 
 Requirements analysis 
 Software design 
 Software construction 
 Software deployment 
 Software engineering 
 Software maintenance 
 Programming team 
 Open source model 
 Theory of computing 
 Model of computation 
 Stochastic 
 Formal language 
 Automata theory 
 Computability theory 
 Computational complexity theory 
 Logic 
 Semantics 
 Algorithms 
 Algorithm design 
 Analysis of algorithms 
 Algorithmic efficiency 
 Randomized algorithm 
 Computational geometry 
 Mathematics of computing 
 Discrete mathematics 
 Probability 
 Statistics 
 Mathematical software 
 Information theory 
 Mathematical analysis 
 Numerical analysis 
 Theoretical computer science 
 Computational problem 
 Information systems 
 Database management 
 Information storage 
 Enterprise information 
 Social information 
 Geographic information 
 Decision support 
 Process control 
 Multimedia information 
 Data mining 
 Digital library 
 Computing platform 
 Digital marketing 
 World Wide Web 
 Information retrieval 
 Security 
 Cryptography 
 Formal methods 
 Security hacker 
 Security services 
 Intrusion detection system 
 Hardware security 
 Network security 
 Information security 
 Application security 
 Human- centered computing 
 Accessibility 
 Extended reality 
 augmented 
 virtual 
 Human–computer interaction 
 Interaction design 
 Mobile computing 
 Social computing 
 Ubiquitous computing 
 Visualization 
 Concurrency 
 Concurrent computing 
 Parallel computing 
 Distributed computing 
 Multithreading 
 Multiprocessing 
 Artificial intelligence 
 Computational intelligence 
 Natural language processing 
 Knowledge representation and reasoning 
 Computer vision 
 Automated planning and scheduling 
 Search methodology 
 Control method 
 Philosophy of 
 Distributed 
 Machine learning 
 Supervised 
 Unsupervised 
 Reinforcement 
 Multi-task 
 Cross-validation 
 Graphics 
 Animation 
 Rendering 
 Photograph manipulation 
 Graphics processing unit 
 Image compression 
 Solid modeling 
 Applied computing 
 Quantum computing 
 E-commerce 
 Enterprise software 
 Computational mathematics 
 Computational physics 
 Computational chemistry 
 Computational biology 
 Computational social science 
 Computational engineering 
 Differentiable computing 
 Computational healthcare 
 Digital art 
 Electronic publishing 
 Cyberwarfare 
 Electronic voting 
 Video games 
 Word processing 
 Operations research 
 Educational technology 
 Document management 
 Outline 
 Glossaries 
 Category 
 Authority control databases : National komunikační protokoly</span>"}]]}'> Czech Republic 
 Retrieved from " https://en.wikipedia.org/w/index.php?title=Communication_protocol&oldid=1368747784 " 
 Categories : Communications protocols Data transmission Network protocols Hidden categories: Articles with short description Short description matches Wikidata Use American English from March 2020 All Wikipedia articles written in American English Use dmy dates from December 2022 All articles with unsourced statements Articles with unsourced statements from April 2026 All articles lacking reliable references Articles lacking reliable references from September 2018 All articles with vague or ambiguous time Vague or ambiguous time from March 2022 Articles with unsourced statements from March 2022 Webarchive template wayback links 
 This page was last edited on 10 August 2026, at 21:49  (UTC) . 
 Page was rendered with Parsoid . 
 Text is available under the Creative Commons Attribution-ShareAlike 4.0 License ;
additional terms may apply. By using this site, you agree to the Terms of Use and Privacy Policy . Wikipedia® is a registered trademark of the Wikimedia Foundation, Inc. , a non-profit organization. 
 Privacy policy 
 About Wikipedia 
 Disclaimers 
 Contact Wikipedia 
 Legal & safety contacts 
 Code of Conduct 
 Developers 
 Statistics 
 Cookie statement 
 Mobile view 
 Search 
 Search 
 Toggle the table of contents 
 Communication protocol 
 68 languages 
 Add topic