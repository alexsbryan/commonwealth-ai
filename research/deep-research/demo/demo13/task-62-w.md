Quantum computing - Wikipedia 
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
 History 
 2 
 Quantum information processing 
 Toggle Quantum information processing subsection 
 2.1 
 Quantum information 
 2.2 
 Unitary operators 
 2.3 
 Quantum parallelism 
 2.4 
 Quantum programming 
 2.4.1 
 Gate array 
 2.4.2 
 Quantum Turing machine 
 2.5 
 Quantum-computing paradigms 
 2.6 
 Noisy intermediate-scale quantum computing 
 3 
 Communication 
 Toggle Communication subsection 
 3.1 
 Quantum communication protocols 
 4 
 Algorithms 
 Toggle Algorithms subsection 
 4.1 
 Simulation of quantum systems 
 4.2 
 Cryptography 
 4.3 
 Search problems 
 4.4 
 Quantum annealing 
 4.5 
 Machine learning 
 4.6 
 AI-assisted algorithm discovery 
 5 
 Engineering 
 Toggle Engineering subsection 
 5.1 
 Challenges 
 5.1.1 
 Coolant 
 5.1.2 
 Decoherence 
 5.2 
 Modular and distributed architectures 
 5.3 
 Quantum supremacy 
 5.4 
 State of affairs: 2020s 
 5.5 
 Physical realizations 
 6 
 Theory 
 Toggle Theory subsection 
 6.1 
 Computability 
 6.2 
 Complexity 
 7 
 List of quantum computers 
 Toggle List of quantum computers subsection 
 7.1 
 Types of quantum computers 
 8 
 See also 
 9 
 Notes 
 10 
 References 
 Toggle References subsection 
 10.1 
 Sources 
 11 
 Further reading 
 Toggle Further reading subsection 
 11.1 
 Textbooks 
 11.2 
 Academic papers 
 12 
 External links 
 Toggle the table of contents 
 Quantum computing 
 44 languages 
 Afrikaans अंगिका العربية Asturianu भोजपुरी বাংলা Bosanski Català Dansk Deutsch Ελληνικά Esperanto Español Euskara فارسی Suomi Français Gaeilge עברית हिन्दी Bahasa Indonesia Italiano 日本語 Taqbaylit 한국어 Монгол ꯃꯤꯇꯩ ꯂꯣꯟ Nederlands Português Runa Simi Simple English Shqip Српски / srpski Svenska Kiswahili தமிழ் తెలుగు Türkçe Українська اردو Tiếng Việt 粵語 中文 IsiZulu 
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
 Wikimedia Commons Wikiquote Wikiversity Wikidata item 
 Appearance 
 move to sidebar 
 hide 
 From Wikipedia, the free encyclopedia 
 Computer hardware technology that uses quantum mechanics 
 IBM quantum computer demo at ITU WTSA 2024 in Delhi 
 Bloch sphere representation of a qubit. The state 
 | 
 ψ 
 ⟩ 
 = 
 α 
 | 
 0 
 ⟩ 
 + 
 β 
 | 
 1 
 ⟩ 
 {\displaystyle |\psi \rangle =\alpha |0\rangle +\beta |1\rangle } 
 is a point on the surface of the sphere, partway between the poles, 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 and 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 . 
 A quantum computer is a computer that represents and processes information using quantum states . Quantum computations exploit phenomena such as superposition , interference , and entanglement . Quantum computers have the potential to complete some calculations exponentially faster than classical computers. For example, a large-scale quantum computer could break widely used encryption schemes and aid physicists in performing physical simulations . However, current hardware implementations of quantum computation are largely experimental and suitable for only certain specialized tasks. 
 The basic unit of information in quantum computing, the qubit (quantum bit), serves a similar function as the bit in ordinary or "classical" computing. [ 1 ] Unlike a classical bit, which can be in one of two states (a binary ), a qubit can exist in a linear combination of states known as a quantum superposition . The result of measuring a qubit is one of the two states, given by a probabilistic rule . If a quantum computer manipulates the qubit in a particular way, wave interference effects amplify the probability of the desired measurement result. Quantum algorithm design involves creating procedures that allow a quantum computer to perform this amplification. 
 Quantum computers are not yet practical for real-world applications. If a physical qubit is not sufficiently isolated from its environment, it suffers from quantum decoherence , introducing noise (error) into calculations. Governments have invested in research aimed at developing qubits with longer coherence times and lower error rates. Example implementations include superconductors (which isolate an electrical current by eliminating electrical resistance ) and ion traps (which confine a single atomic particle using electromagnetic fields ). Researchers have claimed that quantum devices can outperform classical computers on specific tasks, a metric referred to as quantum advantage or quantum supremacy . Such tasks are not necessarily useful for real-world applications. As a result, as of 2026 demonstrations are best understood as scientific milestones rather than evidence for near-term deployment. Global government investment in quantum computing reached $10 billion by April 2025. [ 2 ] 
 History [ edit ] 
 For a chronological guide, see Timeline of quantum computing and communication . 
 Quantum mechanics and computer science formed distinct academic communities until the advent of quantum computing. [ 3 ] Quantum theory was developed in the 1920s to explain perplexing physical phenomena. [ 4 ] [ 5 ] Computers emerged decades later. [ 6 ] Both disciplines had practical applications during World War II ; computers played a major role in wartime cryptography , [ 7 ] while quantum physics was essential for nuclear physics , e.g., in the Manhattan Project . [ 8 ] 
 As physicists applied quantum mechanical models to computational problems and swapped bits for qubits , quantum mechanics and computer science began to converge. In 1980, Paul Benioff introduced the quantum Turing machine , which used quantum theory to describe a simplified computer. [ 9 ] As digital computers became faster, physicists faced an exponential increase in overhead when simulating quantum dynamics , [ 10 ] prompting Yuri Manin and Richard Feynman to independently suggest that hardware based on quantum phenomena might be more efficient for computer simulation. [ 11 ] [ 12 ] [ 13 ] In a 1984 paper, Charles Bennett and Gilles Brassard applied quantum theory to cryptography protocols and demonstrated that quantum key distribution could enhance information security . [ 14 ] [ 15 ] 
 Quantum algorithms then emerged for solving oracle problems , such as Deutsch's algorithm in 1985, [ 16 ] the –</span>Vazirani algorithm"}]]}'>Bernstein–Vazirani algorithm in 1993, [ 17 ] and Simon's algorithm in 1994. [ 18 ] These algorithms did not solve practical problems, but demonstrated mathematically that more information could be obtained by querying a black box with a quantum state in superposition , sometimes referred to as quantum parallelism. [ 19 ] 
 Peter Shor (pictured here in 2017) showed in 1994 that a scalable quantum computer would be able to break RSA encryption . 
 Peter Shor built on these results in 1994 with polynomial-time quantum algorithms for integer factorization and the discrete logarithm problem . [ 20 ] A sufficiently large quantum computer could therefore break widely used public-key cryptography systems: efficient factorization would compromise RSA , while an efficient discrete-logarithm algorithm would compromise Diffie–Hellman key exchange . The cryptographic implications of Shor's algorithm drew attention to quantum computing. In 1996, Grover's algorithm established a quantum speedup for the unstructured search problem. [ 21 ] [ 22 ] The same year, Seth Lloyd proved that quantum computers could simulate quantum systems without the exponential overhead required by classical simulations, [ 23 ] validating Feynman's 1982 conjecture. [ 24 ] 
 Experimentalists constructed small-scale quantum computers using trapped ions and superconductors. [ 25 ] In 1998, a two-qubit quantum computer demonstrated technical feasibility. [ 26 ] [ 27 ] Subsequent experiments increased the number of qubits and reduced error rates. [ 25 ] 
 In 2019, Google AI and NASA announced that they had achieved quantum supremacy with a 54-qubit machine, performing a computation that classical supercomputers would take an estimated 10,000 years to complete—a claim subsequently disputed by IBM , which argued the calculation could be done in approximately 2.5 days on its Summit supercomputer with optimized algorithms, sparking a debate over the threshold for this milestone. [ 28 ] [ 29 ] [ 30 ] [ 31 ] [ 32 ] 
 Quantum computing increasingly focused on controlling decoherence through quantum error correction. In 2024, researchers demonstrated approaches for high threshold, low-overhead fault-tolerant quantum memory. These developments represented a critical step toward scaling systems beyond the noisy intermediate-scale quantum (NISQ) era into reliable, fault-tolerant computing architectures, though large-scale physical implementation remains an engineering challenge. [ 33 ] 
 Quantum information processing [ edit ] 
 Computer engineers typically describe a modern computer 's operation in terms of classical electrodynamics . In these computers, components, such as semiconductors and random number generators , may rely on quantum behavior; however, because they are not isolated from their environment, any quantum information eventually quickly decoheres . While programmers may depend on probability theory when designing a randomized algorithm , quantum-mechanical notions such as superposition and wave interference are largely irrelevant in program analysis . 
 The "classical" in classical computation thus refers to the computational model, not to whether the microscopic physics of the hardware is ultimately quantum-mechanical. A conventional digital computer can be described by classical states and transition rules: memory stores bits, while logic elements transform one configuration of bits into another. This computational behavior is not tied to electronics, and can be abstracted through the idea of a Turing machine , a mechanical device that performs deterministic transformations on a finite state. In principle, the same classical transition rules can be implemented by some entirely classical mechanical device, possibly with a fixed slow-down in physical time. [ 34 ] If a classical computation uses randomness, this can be modeled as access to random classical bits rather than as coherent quantum information. [ 35 ] A quantum computer, by contrast, uses coherent quantum states, so that superposition, relative phase, and interference are part of the computation itself, and have no classical counterpart. 
 Quantum programs instead rely on precise control of coherent quantum systems. Physicists describe these systems mathematically using linear algebra . Complex numbers model probability amplitudes , vectors model quantum states , and matrices model the operations that can be performed on these states. Programming a quantum computer is then a matter of composing operations in such a way that the resulting program computes a useful result in theory and is implementable in practice. 
 Physicist Charlie Bennett noted that since classical computers are composed of quantum atoms, one might study them from the opposite direction: [ 36 ] 
 A classical computer is a quantum computer   ... so we shouldn't be asking about "where do quantum speedups come from?" We should say, "Well, all computers are quantum.   ... Where do classical slowdowns come from?" 
 Quantum information [ edit ] 
 The bit is the basic concept of classical information theory. A bit is in one of two physical states, typically denoted 0 and 1. 
 The qubit is the unit of quantum information . A qubit is an abstract mathematical model that applies to any physical system that is represented by that model. When measured, a qubit takes one of two states 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 or 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 . However, the quantum states 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 and 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 belong to a vector space , meaning that they can be multiplied by constants and added together, returning a valid quantum state. Such a combination is known as a superposition of 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 and 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 . [ 37 ] [ 38 ] 
 A two-dimensional vector mathematically represents a qubit state. Physicists typically use bra–ket notation for quantum mechanical linear algebra , writing 
 | 
 ψ 
 ⟩ 
 {\displaystyle |\psi \rangle } 
 ' ket psi ' for a vector labeled 
 ψ 
 {\displaystyle \psi } 
 . Because a qubit is a two-state system, any qubit state takes the form 
 α 
 | 
 0 
 ⟩ 
 + 
 β 
 | 
 1 
 ⟩ 
 {\displaystyle \alpha |0\rangle +\beta |1\rangle } 
 , where 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 and 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 are the standard basis states, [ a ] and 
 α 
 {\displaystyle \alpha } 
 and 
 β 
 {\displaystyle \beta } 
 are the probability amplitudes , which are in general complex numbers . [ 38 ] If either 
 α 
 {\displaystyle \alpha } 
 or 
 β 
 {\displaystyle \beta } 
 is zero, the qubit is effectively a classical bit; when both are nonzero, the qubit is in superposition. Such a quantum state vector behaves similarly to a (classical) probability vector , with one key difference: unlike probabilities, probability amplitudes are not necessarily positive numbers. [ 40 ] Negative amplitudes allow for destructive wave interference. 
 When a qubit is measured in the standard basis , the result is a classical bit. The Born rule describes the norm-squared correspondence between amplitudes and probabilities — when measuring a qubit 
 α 
 | 
 0 
 ⟩ 
 + 
 β 
 | 
 1 
 ⟩ 
 {\displaystyle \alpha |0\rangle +\beta |1\rangle } 
 , the state collapses to 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 with probability 
 | 
 α 
 | 
 2 
 {\displaystyle |\alpha |^{2}} 
 , or to 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 with probability 
 | 
 β 
 | 
 2 
 {\displaystyle |\beta |^{2}} 
 .
Any valid qubit state has coefficients 
 α 
 {\displaystyle \alpha } 
 and 
 β 
 {\displaystyle \beta } 
 such that 
 | 
 α 
 | 
 2 
 + 
 | 
 β 
 | 
 2 
 = 
 1 
 {\displaystyle |\alpha |^{2}+|\beta |^{2}=1} 
 . As an example, measuring the qubit 
 1 
 / 
 2 
 | 
 0 
 ⟩ 
 + 
 1 
 / 
 2 
 | 
 1 
 ⟩ 
 {\displaystyle 1/{\sqrt {2}}|0\rangle +1/{\sqrt {2}}|1\rangle } 
 would produce either 
 | 
 0 
 ⟩ 
 {\displaystyle |0\rangle } 
 or 
 | 
 1 
 ⟩ 
 {\displaystyle |1\rangle } 
 with equal probability. 
 Two particularly important superposition states are the plus state 
 | 
 + 
 ⟩ 
 = 
 1 
 / 
 2 
 | 
 0 
 ⟩ 
 + 
 1 
 / 
 2 
 | 
 1 
 ⟩ 
 {\displaystyle |+\rangle =1/{\sqrt {2}}|0\rangle +1/{\sqrt {2}}|1\rangle } 
 and the minus state 
 | 
 − 
 ⟩ 
 = 
 1 
 / 
 2 
 | 
 0 
 ⟩ 
 − 
 1 
 / 
 2 
 | 
 1 
 ⟩ 
 {\displaystyle |-\rangle =1/{\sqrt {2}}|0\rangle -1/{\sqrt {2}}|1\rangle } 
 . While both yield outcomes 0 and 1 with equal probability upon standard basis measurement, they behave differently under operations such as the Hadamard gate —which maps 
 | 
 0 
 ⟩ 
 ↔ 
 | 
 + 
 ⟩ 
 {\displaystyle |0\rangle \leftrightarrow |+\rangle } 
 and 
 | 
 1 
 ⟩ 
 ↔ 
 | 
 − 
 ⟩ 
 {\displaystyle |1\rangle \leftrightarrow |-\rangle } 
 —demonstrating that relative phase differences carry meaningful quantum information. 
 Each additional qubit doubles the dimension of the state space . [ 39 ] As an example, the vector ⁠ 1 / √2 ⁠ | 00 ⟩ + ⁠ 1 / √2 ⁠ | 01 ⟩ represents a two-qubit state, a tensor product of the qubit | 0 ⟩ with the qubit ⁠ 1 / √2 ⁠ | 0 ⟩ + ⁠ 1 / √2 ⁠ | 1 ⟩ . This vector inhabits a four-dimensional vector space spanned by the basis vectors | 00 ⟩ , | 01 ⟩ , | 10 ⟩ , and | 11 ⟩ . 
 In general, the vector space for an n -qubit system is 2 n -dimensional, and this makes it challenging for a classical computer to simulate a quantum one: representing a 100-qubit system requires storing 2 100 classical values. 
 Unitary operators [ edit ] 
 See also: Unitarity (physics) 
 The state of this one-qubit quantum memory can be manipulated by applying quantum logic gates , analogous to how classical memory can be manipulated with classical logic gates . One important gate for both classical and quantum computation is the NOT gate, which can be represented by a matrix 
 X 
 := 
 ( 
 0 
 1 
 1 
 0 
 ) 
 . 
 {\displaystyle X:={\begin{pmatrix}0&1\\1&0\end{pmatrix}}.} 
 Mathematically, the application of such a logic gate to a quantum state vector is modeled with matrix multiplication . Thus 
 X 
 | 
 0 
 ⟩ 
 = 
 | 
 1 
 ⟩ 
 {\displaystyle X|0\rangle =|1\rangle } 
 and 
 X 
 | 
 1 
 ⟩ 
 = 
 | 
 0 
 ⟩ 
 {\displaystyle X|1\rangle =|0\rangle } 
 . 
 The mathematics of single-qubit gates can be extended to operate on multi-qubit quantum memories in two important ways. One way is to select a qubit and apply that gate to the target qubit while leaving the remainder of the memory unaffected. Another way is to apply the gate to its target only if another part of the memory is in a desired state. These two choices can be illustrated using another example. The possible states of a two-qubit quantum memory are: 
 | 
 00 
 ⟩ 
 := 
 ( 
 1 
 0 
 0 
 0 
 ) 
 ; 
 | 
 01 
 ⟩ 
 := 
 ( 
 0 
 1 
 0 
 0 
 ) 
 ; 
 | 
 10 
 ⟩ 
 := 
 ( 
 0 
 0 
 1 
 0 
 ) 
 ; 
 | 
 11 
 ⟩ 
 := 
 ( 
 0 
 0 
 0 
 1 
 ) 
 . 
 {\displaystyle |00\rangle   :={\begin{pmatrix}1\\0\\0\\0\end{pmatrix}};\quad |01\rangle   :={\begin{pmatrix}0\\1\\0\\0\end{pmatrix}};\quad |10\rangle   :={\begin{pmatrix}0\\0\\1\\0\end{pmatrix}};\quad |11\rangle   :={\begin{pmatrix}0\\0\\0\\1\end{pmatrix}}.}
 The controlled NOT (CNOT) gate can then be represented using the following matrix: 
 CNOT 
 := 
 ( 
 1 
 0 
 0 
 0 
 0 
 1 
 0 
 0 
 0 
 0 
 0 
 1 
 0 
 0 
 1 
 0 
 ) 
 . 
 {\displaystyle \operatorname {CNOT}   :={\begin{pmatrix}1&0&0&0\\0&1&0&0\\0&0&0&1\\0&0&1&0\end{pmatrix}}.}
 As a mathematical consequence of this definition, 
 CNOT 
 ⁡ 
 | 
 00 
 ⟩ 
 = 
 | 
 00 
 ⟩ 
 {\textstyle \operatorname {CNOT} |00\rangle =|00\rangle } 
 , 
 CNOT 
 ⁡ 
 | 
 01 
 ⟩ 
 = 
 | 
 01 
 ⟩ 
 {\textstyle \operatorname {CNOT} |01\rangle =|01\rangle } 
 , 
 CNOT 
 ⁡ 
 | 
 10 
 ⟩ 
 = 
 | 
 11 
 ⟩ 
 {\textstyle \operatorname {CNOT} |10\rangle =|11\rangle } 
 , and 
 CNOT 
 ⁡ 
 | 
 11 
 ⟩ 
 = 
 | 
 10 
 ⟩ 
 {\textstyle \operatorname {CNOT} |11\rangle =|10\rangle } 
 . In other words, the CNOT applies a NOT gate ( 
 X 
 {\textstyle X} 
 from before) to the second qubit if and only if the first qubit is in the state 
 | 
 1 
 ⟩ 
 {\textstyle |1\rangle } 
 . If the first qubit is 
 | 
 0 
 ⟩ 
 {\textstyle |0\rangle } 
 , nothing is done to either qubit. 
 In summary, quantum computation can be described as a network of quantum logic gates and measurements. However, any measurement can be deferred to the end of quantum computation, though this deferment may come at a computational cost, so most quantum circuits depict a network consisting only of quantum logic gates and no measurements. 
 Quantum parallelism [ edit ] 
 Quantum parallelism is a heuristic that quantum computers can be thought of as evaluating a function for multiple input values simultaneously. This can be achieved by preparing a quantum system in a superposition of input states and applying a unitary transformation that encodes the function to be evaluated. The resulting state encodes the function's output values for all input values in the superposition, enabling the simultaneous computation of multiple outputs. This property is key to the acceleration of many quantum algorithms. However, parallelism in this sense is insufficient to speed up a computation, because the measurement at the end of the computation gives only one value. To be useful, a quantum algorithm must also incorporate some other conceptual ingredient. [ 19 ] [ 41 ] 
 Quantum programming [ edit ] 
 Further information: Quantum programming 
 Various models of computation are used for quantum computing, distinguished by the basic elements in which the computation is decomposed. 
 Gate array [ edit ] 
 A quantum circuit diagram implementing a Toffoli gate from more primitive gates 
 A quantum gate array decomposes computation into a sequence of few-qubit quantum gates . A quantum computation can be described as a network of quantum logic gates and measurements. Any measurement can be deferred to the end of quantum computation, though this deferment may come at a computational cost, so most quantum circuits depict a network consisting only of quantum logic gates and no measurements. 
 Any quantum computation (in the above formalism, any unitary matrix of size 
 2 
 n 
 × 
 2 
 n 
 {\displaystyle 2^{n}\times 2^{n}} 
 over 
 n 
 {\displaystyle n} 
 qubits) can be represented as a network of quantum logic gates from a fairly small family of gates. A choice of gate family that enables this construction is known as a universal gate set , since a computer that can run such circuits is a universal quantum computer . One common such set includes all single-qubit gates as well as the CNOT gate. This means any quantum computation can be performed by executing a sequence of single-qubit gates together with CNOT gates. Though this gate set is infinite, it can be replaced with a finite gate set by appealing to the Solovay-Kitaev theorem . Implementation of Boolean functions using the few-qubit quantum gates is presented here. [ 42 ] 
 Quantum Turing machine [ edit ] 
 A quantum Turing machine is the quantum analog of a Turing machine . [ 9 ] All of these models of computation—quantum circuits, [ 43 ] one-way quantum computation , [ 44 ] adiabatic quantum computation, [ 45 ] and topological quantum computation [ 46 ] —have been shown to be equivalent to the quantum Turing machine; given a perfect implementation of one such quantum computer, it can simulate all the others with no more than polynomial overhead. This equivalence need not hold for practical quantum computers, since the overhead of simulation may be too large to be practical. 
 Quantum-computing paradigms [ edit ] 
 A measurement-based quantum computer decomposes computation into a sequence of Bell state measurements and single-qubit quantum gates applied to a highly entangled initial state (a cluster state ), using a technique called quantum gate teleportation . 
 An adiabatic quantum computer , based on quantum annealing , decomposes computation into a slow continuous transformation of an initial Hamiltonian into a final Hamiltonian, whose ground states contain the solution. [ 47 ] 
 A topological quantum computer decomposes computation into the braiding of anyons in a 2D lattice. [ 48 ] 
 Noisy intermediate-scale quantum computing [ edit ] 
 The threshold theorem shows how increasing the number of qubits can mitigate errors, [ 49 ] yet fully fault-tolerant quantum computing remains out of reach as of 2026. [ 50 ] According to some researchers, noisy intermediate-scale quantum ( NISQ ) machines may have specialized uses in the near future, but noise in quantum gates limits their reliability. [ 50 ] Scientists at Harvard created "quantum circuits" that correct errors more efficiently than alternative methods, which may remove a major obstacle to practical quantum computers. [ 51 ] The Harvard research team was supported by MIT , QuEra Computing , Caltech , and Princeton and funded by DARPA 's Optimization with Noisy Intermediate-Scale Quantum devices (ONISQ) program. [ 52 ] [ 53 ] 
 Communication [ edit ] 
 Further information: Quantum information science 
 Quantum cryptography enables methods for secure data transmission; for example, quantum key distribution uses entangled quantum states to establish secure cryptographic keys . [ 54 ] :   1017   When a sender and receiver exchange quantum states, they can guarantee that an adversary does not intercept the message, as any eavesdropper would disturb the delicate quantum system and introduce a detectable change. [ 55 ] With appropriate cryptographic protocols , the sender and receiver can privately share information resistant to eavesdropping. [ 14 ] [ 56 ] 
 Modern fiber-optic cables can transmit quantum information over relatively short distances. Ongoing experimental research aims to develop more reliable hardware (such as quantum repeaters), hoping to scale this technology to long-distance quantum networks with end-to-end entanglement. Theoretically, this could enable novel technological applications, such as distributed quantum computing and enhanced quantum sensing . [ 57 ] [ 58 ] 
 Quantum communication protocols [ edit ] 
 This section does not cite any sources . Please help improve this section by adding citations to reliable sources . Unsourced material may be challenged and removed . ( July 2026 ) ( Learn how and when to remove this message ) 
 Quantum teleportation is a protocol by which Alice can transmit the quantum state of a qubit to Bob using one shared entangled pair (e-bit) and two classical bits of communication. The state of Alice's qubit is not physically transmitted—instead, it is reconstructed at Bob's end through classically communicated measurement outcomes and local unitary corrections. This demonstrates that quantum communication requires both entanglement and classical communication; neither alone is sufficient. Teleportation cannot be used to transmit information faster than light because the classical bits must travel through normal channels. 
 Superdense coding is the complementary protocol: using one shared e-bit and sending only one qubit, Alice can transmit two classical bits to Bob. This appears to violate Holevo's theorem —which states that a single qubit can carry at most one bit of classical information—but the shared entanglement circumvents this limit. Superdense coding thus demonstrates that entanglement can effectively double the classical information-carrying capacity of quantum communication. 
 Algorithms [ edit ] 
 See also: List of quantum algorithms 
 Progress in finding quantum algorithms typically focuses on the quantum circuit model, [ 43 ] though exceptions such as the quantum adiabatic algorithm exist. Quantum algorithms can be roughly categorized by the type of speedup achieved over corresponding classical algorithms. [ 59 ] 
 Quantum algorithms that offer more than a polynomial speedup over the best-known classical algorithm include Shor's algorithm for factoring and the related quantum algorithms for computing discrete logarithms , solving Pell's equation , and, more generally, solving the hidden subgroup problem for abelian finite groups. [ 59 ] These algorithms depend on the primitive of the quantum Fourier transform . No mathematical proof has been found that shows that an equally fast classical algorithm cannot be discovered, but evidence suggests that this is unlikely. [ 60 ] Certain oracle problems like Simon's problem and the Bernstein–Vazirani problem do give provable speedups, though this is in the quantum query model , which is a restricted model where lower bounds are much easier to prove and don't necessarily translate to practical problems. 
 Other problems, including the simulation of quantum physical processes from chemistry and solid-state physics, the approximation of certain Jones polynomials , and the quantum algorithm for linear systems of equations , have quantum algorithms appearing to give super-polynomial speedups and are BQP -complete. Because these problems are BQP-complete, an equally fast classical algorithm for them would imply that "no quantum algorithm" provides a super-polynomial speedup, which is unlikely. [ 61 ] 
 In addition to these problems, quantum algorithms are explored for applications in cryptography, optimization, and machine learning, although most of these remain at the research stage and require significant advances in error correction and hardware scalability for practical implementation. [ 62 ] 
 Some quantum algorithms, such as Grover's algorithm and amplitude amplification , give polynomial speedups over corresponding classical algorithms. [ 59 ] Though these algorithms give comparably modest quadratic speedup, they are widely applicable and thus accelerate a wide range of problems. [ 22 ] These improvements are, however, over the theoretical worst-case of classical algorithms, and real-world speed-ups over traditional algorithms have not been demonstrated. 
 Simulation of quantum systems [ edit ] 
 Main article: Quantum simulation 
 Since chemistry and nanotechnology rely on understanding quantum systems, and such systems are impossible to efficiently simulate classically, quantum simulation may be an important application. [ 63 ] Quantum computational chemistry is promising for quantum computing, particularly for problems in electronic structure, chemical dynamics, and spectroscopy; useful implementations remain hardware-limited. [ 64 ] Quantum simulation could be used to simulate the behavior of atoms and particles under unusual conditions such as the reactions inside a collider . [ 65 ] In June 2023, IBM computer scientists reported that a quantum computer produced better results for a physics problem than a conventional supercomputer. [ 66 ] [ 67 ] 
 About 2% of the annual global energy output is used for nitrogen fixation to produce ammonia for the Haber process in the agricultural fertiliser industry. Quantum simulations might be used to understand this process and increase energy efficiency. [ 68 ] [ 69 ] [ 70 ] [ 71 ] 
 Cryptography [ edit ] 
 Main articles: Post-quantum cryptography and Quantum cryptography 
 Digital cryptography enables communications to remain private, preventing unauthorized parties from accessing them. Conventional encryption, the obscuring of a message with a key through an algorithm, relies on the algorithm being difficult to reverse. Encryption underlies digital signatures and authentication mechanisms. Quantum computing may be sufficiently more powerful that difficult reversals are feasible, allowing messages relying on conventional encryption to be read. [ 72 ] 
 Thus quantum computing can in theory be used to attack currently-used cryptographic systems. Integer factorization , which underpins the security of public key cryptographic systems, is believed to be computationally infeasible on a classical computer for large integers that are the product of a few prime numbers (e.g., the product of two 300-digit primes). [ 73 ] By contrast, a quantum computer could solve this problem exponentially faster using Shor's algorithm to factor the integer. [ 74 ] This would allow a quantum computer to break many widely used cryptographic systems, in the sense that a polynomial time (in the number of digits of the integer) algorithm could do so. In particular, most popular public key ciphers rely on the difficulty of factoring integers or the discrete logarithm . In particular, RSA , Diffie–Hellman , and elliptic curve Diffie–Hellman algorithms could be broken. These are used to secure Web pages, encrypted emails, and many other data. Breaking these would have significant ramifications for electronic privacy and security. 
 Identifying cryptographic systems that are secure against quantum algorithms is an actively researched topic under the field of post-quantum cryptography . [ 75 ] [ 76 ] Some public-key algorithms are based on problems that Shor's algorithm cannot solve, such as the McEliece cryptosystem , which relies on a hard problem in coding theory . [ 75 ] [ 77 ] Lattice-based cryptosystems are not known to be susceptible to quantum computers, and finding a polynomial time algorithm for solving the dihedral hidden subgroup problem , which would break many lattice-based cryptosystems, is a well-studied open problem. [ 78 ] Applying Grover's algorithm to break a symmetric (secret-key) algorithm by brute force requires roughly 2 n /2 invocations of the underlying cryptographic algorithm, compared with roughly 2 n in the classical case, [ 79 ] meaning that symmetric key lengths are effectively halved: AES-256 would have comparable security against such an attack to that of AES-128 against classical brute-force search. 
 Post-quantum algorithms are designed to run but be difficult to break on a classical computer. Quantum cryptography replaces conventional encryption algorithms with techniques based on quantum mechanics such as entanglement. In principle, quantum encryption cannot be decoded even by a quantum computer. This advantage comes at a significant infrastructure cost, while effectively preventing legitimate decoding of messages. [ 72 ] 
 Search problems [ edit ] 
 Main article: Grover's algorithm 
 The most well-known example of a problem that allows for a polynomial quantum speedup is unstructured search, which involves finding a marked item out of a list of 
 n 
 {\displaystyle n} 
 items in a database. This can be solved by Grover's algorithm using 
 O 
 ( 
 n 
 ) 
 {\displaystyle O({\sqrt {n}})} 
 queries to the database, quadratically fewer than the 
 Ω 
 ( 
 n 
 ) 
 {\displaystyle \Omega (n)} 
 queries required for classical algorithms. In this case, the advantage is not only provable but also optimal: it has been shown that Grover's algorithm gives the maximal possible probability of finding the desired element for any number of oracle lookups. Many examples of provable speedups for query problems are based on Grover's algorithm, including Brassard, Høyer, and Tapp's algorithm for finding collisions in two-to-one functions, [ 80 ] and Farhi, Goldstone, and Gutmann's algorithm for evaluating NAND trees. [ 81 ] 
 Problems that can be efficiently addressed with Grover's algorithm have the following properties: [ 82 ] [ 83 ] 
 The collection of possible answers has no searchable structure 
 The number of possible answers to check is the same as the number of inputs to the algorithm, 
 A Boolean function exists that evaluates each input and determines whether it is the correct answer. 
 For problems with all these properties, the running time of Grover's algorithm on a quantum computer scales as the square root of the number of inputs (or elements in the database), as opposed to the linear scaling of classical algorithms. A general class of problems to which Grover's algorithm can be applied [ 84 ] is a Boolean satisfiability problem , in which the algorithm iterates through all possible answers. An example and possible application of this is a password cracker that attempts to guess a password. Breaking symmetric ciphers with this algorithm is of interest to government agencies. [ 85 ] 
 Quantum annealing [ edit ] 
 A wafer of adiabatic quantum computers Quantum annealing uses the adiabatic theorem to perform calculations. A system is placed in the ground state for a simple Hamiltonian, which evolves to a more complicated Hamiltonian whose ground state represents the solution to the problem in question. The adiabatic theorem states that if the evolution is slow enough, the system stays in its ground state throughout the process. Quantum annealing can solve Ising models and the (computationally equivalent) quadratic unconstrained binary optimisation (QUBO) problem, which in turn can be used to encode a wide range of combinatorial optimization problems. [ 86 ] Adiabatic optimization may be helpful for solving computational biology problems. [ 87 ] 
 Machine learning [ edit ] 
 Main article: Quantum machine learning 
 Since quantum computers can produce outputs that classical computers cannot produce efficiently, and since quantum computation is fundamentally linear algebra, so quantum algorithms that can speed up machine learning tasks may be possible. [ 50 ] [ 88 ] However, review literature notes that many proposed quantum machine-learning advantages rely on assumptions about efficient data encoding or continued access to quantum hardware, and have not translated into practical advantage as of 2024. [ 89 ] [ 90 ] For example, the HHL Algorithm is believed to provide speedup over classical counterparts. [ 50 ] [ 91 ] Research groups have explored quantum annealing hardware for training Boltzmann machines and deep neural networks . [ 92 ] [ 93 ] [ 94 ] 
 Deep generative chemistry models have been explored for potential applications in drug discovery . Near-term quantum hardware has been explored for molecular generative modeling for drug discovery. In 2023, researchers reported a hybrid quantum–classical generative model based on a restricted Boltzmann machine, implemented on a commercially available quantum annealing device, to generate novel small molecules with physicochemical properties comparable to medicinal compounds. [ 95 ] [ 96 ] However, the immense size and complexity of the structural space of all possible relevant molecules pose significant obstacles, which could be overcome in the future by quantum computers. Quantum computers are naturally good for solving complex quantum many-body problems [ 23 ] and thus may apply to applications involving quantum chemistry. Quantum-enhanced generative models [ 97 ] including quantum generative adversarial networks (GANs) [ 98 ] may be developed into generative chemistry algorithms. 
 AI-assisted algorithm discovery [ edit ] 
 Artificial intelligence has been explored as a tool for discovering and optimizing quantum algorithms. AlphaEvolve , a Google DeepMind system based on large language models and evolutionary algorithms , has been described as a coding agent for scientific and algorithmic discovery. [ 99 ] In quantum-computing research, AlphaEvolve-optimized quantum circuits have been used in work on quantum computation of molecular geometry through many-body nuclear spin echoes . [ 100 ] 
 Engineering [ edit ] 
 As of 2023 , [ update ] classical computers outperformed quantum computers for all real-world applications. [ 101 ] [ 102 ] 
 Challenges [ edit ] 
 Many technical challenges confront the building a large-scale quantum computer. [ 103 ] Physicist David DiVincenzo has listed these requirements for a practical quantum computer: [ 104 ] 
 Physically scalable to increase the number of qubits 
 Qubits that can be initialized to arbitrary values 
 Quantum gates that are faster than decoherence time 
 Universal gate set 
 Qubits that are easily read. 
 The control of multi-qubit systems requires the rapid generation and coordination of a large number of electrical signals with deterministic timing. This has led to the development of quantum controllers that enable interfacing with the qubits. Scaling these systems to support many qubits is an additional challenge. [ 105 ] 
 The potential to break public-key encryption has motivated changes in global cybersecurity strategies. The National Institute of Standards and Technology (NIST) initiated detailed standardization processes for post-quantum cryptography. These global efforts are designed to develop, evaluate, and deploy cryptographic algorithms that remain safe against both quantum and classical attacks. [ 106 ] 
 Coolant [ edit ] 
 Sourcing parts for quantum computers is difficult. Superconducting quantum computers , such as those constructed by Google and IBM , need helium-3 , a nuclear research byproduct, and special superconducting cables made only by one company, Coax Co. [ 107 ] On 27 January 2026, DARPA called for proposals for a quantum computing coolant below 1 kelvin , which does not use helium-3. In February 2026, the Chinese Academy of Sciences announced the testing of a rare-earth alloy, Eu Co 2 Al 9 , which could fill a similar role. [ 108 ] 
 Decoherence [ edit ] 
 Quantum decoherence must be controlled or eliminated. Error rates are typically proportional to the ratio of operating time to decoherence time; hence, any operation must be completed much more quickly than the decoherence time. [ citation needed ] This usually means isolating the system from its environment, as external interactions cause decoherence. However, other sources also exist. Examples include the quantum gates, the lattice vibrations, and the background thermonuclear spin of the physical system that implements the qubits. Decoherence is irreversible, as it is effectively non-unitary, and must be controlled or avoided. Decoherence times for candidate systems in particular, the transverse relaxation time T 2 (for NMR and MRI technology, also called the dephasing time), typically range between nanoseconds and seconds at low temperatures. [ 109 ] Some quantum computers require their qubits to be cooled to 20 millikelvin (usually using a dilution refrigerator [ 110 ] ) in order to prevent significant decoherence. [ 111 ] A 2020 study reported that ionizing radiation such as cosmic rays can cause certain systems to decohere within milliseconds. [ 112 ] 
 As a result, time-consuming tasks may render some quantum algorithms inoperable, as maintaining the state of qubits over a long period eventually corrupts the superpositions. [ 113 ] 
 These issues are more difficult for optical approaches as the timescales are orders of magnitude shorter. An often-cited approach to overcoming them is optical pulse shaping . 
 As described by the threshold theorem , if the error rate is small enough, it is thought to be possible to use quantum error correction to suppress errors and decoherence. This allows the total calculation time to be longer than the decoherence time if the error correction scheme can correct errors faster than decoherence introduces them. An often-cited figure for the required error rate in each gate for fault-tolerant computation is 10 −3 , assuming the noise is depolarizing. [ citation needed ] 
 Meeting this scalability condition is possible for a wide range of systems. However, error correction requires far more qubits. The number required to factor integers using Shor's algorithm is still polynomial, and thought to be between L and L 2 , where L is the number of binary digits in the number to be factored; error correction algorithms would inflate this figure by an additional factor of L . For a 1000-bit number, this implies a need for about 10 4 bits without error correction. [ 114 ] With error correction, the figure would rise to about 10 7 bits. Computation time is about L 2 or about 10 7 steps and at 1   MHz, about 10 seconds. However, the encoding and error-correction overheads increase the size of a real fault-tolerant quantum computer by orders of magnitude. Estimates [ 115 ] [ 116 ] show that at least 3   million physical qubits would factor a 2,048-bit integer in 5 months on a fully error-corrected trapped-ion quantum computer. In terms of the number of physical qubits, to date, this remains the lowest estimate [ 117 ] for practically useful integer factorization problem sizing 1,024-bit or larger. 
 One approach to overcoming errors combines low-density parity-check code with cat qubits that have intrinsic bit-flip error suppression. Implementing 100 logical qubits with 768 cat qubits could reduce the error rate to one part in 10 8 per cycle per bit. [ 118 ] 
 Another approach to the stability-decoherence problem is to create a topological quantum computer with anyons , quasi-particles used as threads, and relying on braid theory to form stable logic gates. [ 119 ] [ 120 ] Non-Abelian anyons can, in effect, remember how they have been manipulated, making them potentially useful in quantum computing. [ 121 ] As of 2025, Microsoft and other organizations were investing in quasi-particle research. [ 121 ] 
 Modular and distributed architectures [ edit ] 
 See also: Quantum network 
 One approach to the scalability problem is to distribute a computation across multiple smaller quantum processing modules instead of increasing the number of qubits in a single device. In such modular architectures — also referred to as distributed quantum computing (DQC) —
each module contains a limited number of qubits, and the modules are interconnected through quantum channels (for example, optical fibres) and classical communication links, forming a single logical computing system. [ 122 ] 
 In one strategy, the quantum logic between qubits in different modules is applied using quantum gate teleportation , using remote entanglement between the modules, but local operations and measurements within each module, and classical communication of measurement outcomes. [ 123 ] Quantum algorithms distributed across a photonic network link between trapped-ion modules, as well as teleported two-qubit gates between remote solid-state qubit registers based on nitrogen-vacancy centers in diamond, were demonstrated in 2025. [ 122 ] [ 124 ] 
 High rate and high fidelity remote entanglement generation across the network is the core challenge in distributed protocols. Quantum sensing may became integral to a distributed quantum computer. [ 125 ] 
 Quantum supremacy [ edit ] 
 John Preskill coined the term quantum supremacy to describe the engineering feat of demonstrating that a programmable quantum device can solve a problem beyond the capabilities of classical computers. [ 126 ] [ 50 ] [ 127 ] The problem need not be useful, so quantum supremacy test may be just a future benchmark. [ 128 ] 
 In October 2019, Google Quantum AI , with the help of NASA , became the first to claim to have achieved quantum supremacy by performing calculations on the Sycamore quantum computer more than 3,000,000 times faster than they could be done on Summit , then generally considered the world's fastest computer. [ 29 ] [ 129 ] [ 130 ] This claim was challenged: IBM stated that Summit can perform samples much faster than claimed. [ 131 ] [ 132 ] Researchers later developed better quantum algorithms for the sampling problem, [ 133 ] [ 134 ] [ 135 ] possibly beating Summit. [ 136 ] [ 137 ] [ 138 ] 
 In December 2020, a group at USTC implemented a type of boson sampling on 76 photons with a photonic quantum computer , Jiuzhang , seeking quantum supremacy. [ 139 ] [ 140 ] [ 141 ] The authors claimed that a classical computer would require 600 million years to generate the number of samples their quantum processor generated in 20 seconds. [ 142 ] 
 Hyped claims of quantum supremacy, [ 143 ] were based on tasks that do not directly imply real-world applications. [ 101 ] [ 144 ] 
 A January 2024 study reported verification of quantum supremacy experiments by computing exact amplitudes for experimentally generated bitstrings using a Sunway supercomputer, demonstrating a significant leap in simulation capability built on a multiple-amplitude tensor network contraction algorithm. [ 145 ] 
 State of affairs: 2020s [ edit ] 
 Despite high hopes for quantum computing, significant progress in hardware, and optimism about future applications, a 2023 article summarized current quantum computers as "For now, [good for] absolutely nothing". The article claimed that quantum computers are yet of no practical use although they are likely to be useful, someday. [ 101 ] A 2023 article stated that quantum computing algorithms are "insufficient for practical quantum advantage without significant improvements across the software/hardware stack". It foretold that the most promising candidates for achieving speedup with quantum computers are "small-data problems", for example, in chemistry and materials science. It concluded that many application domains, such as machine learning, "will not achieve quantum advantage with current quantum algorithms in the foreseeable future", and it identified I/O constraints that make speedup unlikely for "big data problems, unstructured linear systems, and database search based on Grover's algorithm". [ 102 ] 
 This state of affairs can be traced to several current and long-term considerations. 
 Conventional computer hardware and algorithms are optimized for practical tasks and are still improving rapidly. 
 Quantum computing hardware generates is overwhelmed by noise before completing any useful tasks. 
 Quantum algorithms provide speedup only for some tasks, and matching these tasks with practical applications is challenging. Some of these require resources far beyond those available. [ 146 ] [ 147 ] In particular, processing large amounts of data is a challenge. [ 102 ] 
 Some promising algorithms have been "dequantized", i.e., non-quantum analogues with similar complexity have been found. 
 The overhead of quantum error correction may undermine the speedup offered by many quantum algorithms. [ 102 ] 
 Algorithm complexity analysis may make abstract assumptions that do not hold in applications. For example, input data may not be available encoded in quantum states, and "oracle functions" used in Grover's algorithm often have internal structure that can be exploited for faster algorithms. 
 In particular, building computers with large numbers of qubits may be futile if those qubits are not connected well enough and cannot sustain sufficient entanglement for enough time. Researchers often choose novel tasks to differentiate quantum devices, and to prove lower bounds on the complexity of classical algorithms, but this is not always possible. 
 Bill Unruh doubted the practicality of quantum computers in a paper published in 1994. [ 148 ] Paul Davies argued that a 400-qubit computer would conflict with the cosmological information bound implied by the holographic principle . [ 149 ] Gil Kalai doubted that quantum supremacy would ever be achieved. [ 150 ] [ 151 ] [ 152 ] Physicist Mikhail Dyakonov expressed skepticism of quantum computing as follows: 
 "So the number of continuous parameters describing the state of such a useful quantum computer at any given moment must be... about 10 300 ... Could we ever learn to control the more than 10 300 continuously variable parameters defining the quantum state of such a system? My answer is simple. No, never. " [ 153 ] 
 Physical realizations [ edit ] 
 Further information: List of proposed quantum registers 
 Quantum System One , a quantum computer by IBM from 2019 with 20 superconducting qubits [ 154 ] 
 A practical quantum computer must use a physical system as a programmable quantum register. [ 155 ] Researchers are exploring several technologies as candidates for reliable qubit implementations. [ 156 ] Superconductors and trapped ions are some of the most developed proposals, but experimentalists are considering other hardware possibilities as well. [ 157 ] 
For example, topological quantum computer approaches are being explored for more fault-tolerance computing systems. [ 158 ] 
 The first quantum logic gates were implemented with trapped ions and prototype general-purpose machines with up to 20 qubits have been realized. However, the technology behind these devices combines complex vacuum equipment, lasers, and microwave and radio frequency equipment, making full-scale processors difficult to integrate with standard computing equipment. Moreover, the trapped ion system itself has engineering challenges to overcome. [ 159 ] 
 The largest commercial systems are based on superconductor devices and have scaled to 2000 qubits. However, the error rates for larger machines have been on the order of 5%. Technologically, these devices are all cryogenic and scaling to large numbers of qubits requires wafer-scale integration, a serious engineering challenge by itself. [ 160 ] 
 In addition to cryogenic platforms, room-temperature approaches to spin–photon interfaces have been experimentally demonstrated. In 2025, researchers at Stanford University realized a nanoscale device in which a thin layer of molybdenum diselenide is integrated on a nanostructured silicon substrate, enabling a spin–photon interface that operates at ambient conditions using structured "twisted" light to couple electronic and photonic degrees of freedom. [ 161 ] [ 162 ] Such room-temperature, chip-integrated spin–photon interfaces are being investigated as potential building blocks for heterogeneous quantum networks that combine different qubit modalities and reduce reliance on large cryogenic infrastructures. [ 161 ] [ 163 ] 
 Theory [ edit ] 
 Computability [ edit ] 
 Further information: Computability theory 
 Any computational problem solvable by a classical computer is also solvable by a quantum computer. [ 164 ] Intuitively, this is because all physical phenomena, including the operation of classical computers, can be described using quantum mechanics , which underlies the operation of quantum computers. 
 Conversely, any problem solvable by a quantum computer is also solvable by a classical computer. It is possible to simulate both quantum and classical computers manually with just some paper and a pen, if given enough time. Formally, any quantum or classical computer can be simulated by a Turing machine . Quantum computers provide no computability power over classical computers. Thus, quantum computers cannot solve undecidable problems like the halting problem , and the existence of quantum computers does not disprove the Church–Turing thesis . [ 165 ] 
 Complexity [ edit ] 
 Main article: Quantum complexity theory 
 While quantum computers cannot solve any problems that classical computers cannot already solve, it is suspected that they can solve certain problems faster than classical computers. For instance, it is known that quantum computers can efficiently factor integers , while this is not believed to be the case for classical computers. 
 The class of problems that can be efficiently solved by a quantum computer with bounded error is called BQP , for "bounded error, quantum, polynomial time". More formally, BQP is the class of problems that can be solved by a polynomial-time quantum Turing machine with an error probability of at most 1/3. As a class of probabilistic problems, BQP is the quantum counterpart to BPP ("bounded error, probabilistic, polynomial time"), the class of problems that can be solved by polynomial-time probabilistic Turing machines with bounded error. [ 166 ] 
 B 
 P 
 P 
 ⊆ 
 B 
 Q 
 P 
 {\displaystyle {\mathsf {BPP\subseteq BQP}}} 
 but no proof demonstrates that 
 B 
 Q 
 P 
 ≠ 
 B 
 P 
 P 
 {\displaystyle {\mathsf {BQP\neq BPP}}} 
 , which intuitively would mean that quantum computers offer superior time complexity over classical computers. [ 167 ] 
 The suspected relationship of BQP to several classical complexity classes [ 61 ] 
 The exact relationship of BQP to P , NP , and PSPACE is not known. However, it is known that 
 P 
 ⊆ 
 B 
 Q 
 P 
 ⊆ 
 P 
 S 
 P 
 A 
 C 
 E 
 {\displaystyle {\mathsf {P\subseteq BQP\subseteq PSPACE}}} 
 ; that is, all problems that can be efficiently solved by a classical computer can be efficiently solved by a quantum computer, and all problems that can be efficiently solved by a quantum computer can be solved by a classical computer with polynomial space resources. 
 It is suspected that BQP is a strict superset of P, meaning that problems exist that are efficiently solvable by quantum computers that are not efficiently solvable by classical computers. For instance, integer factorization and the discrete logarithm problem are in BQP and are suspected to be outside of P. On the relationship of BQP to NP, little is known except that NP problems that are not in P are in BQP (integer factorization and the discrete logarithm problem are both in NP, for example). It is suspected that 
 N 
 P 
 ⊈ 
 B 
 Q 
 P 
 {\displaystyle {\mathsf {NP\nsubseteq BQP}}} 
 ; that is, it is believed that some efficiently checkable problems are not efficiently solvable by a quantum computer. A direct consequence is that BQP is disjoint from the class of NP-complete problems (if an NP-complete problem were in BQP, then it would follow from NP-hardness that all problems in NP are in BQP). [ 168 ] 
 List of quantum computers [ edit ] 
 See also: List of quantum processors 
 Hanyuan-1 — 100- qubit neutral atom quantum computer from the Chinese Academy of Sciences in China . [ 169 ] 
 IBM Quantum System One — IBM superconducting quantum-computing system introduced in 2019. [ 170 ] 
 IBM Quantum System Two — modular superconducting system using IBM Heron processors. 
 Jiuzhang — photonic quantum-computing prototype for Gaussian boson sampling . [ 171 ] 
 QpiAI-Indus — 25-qubit superconducting quantum computer from QpiAI in India . [ 172 ] 
 Types of quantum computers [ edit ] 
 Cat qubit quantum computer — proposed approach based on cat-state qubits. 
 Kane quantum computer — proposed silicon -based nuclear spin quantum-computer architecture. 
 Linear optical quantum computing — photonic model using photons and linear optical elements. 
 Neutral atom quantum computer — approach using neutral atoms trapped and controlled with optical techniques. 
 Nuclear magnetic resonance quantum computer — approach using nuclear magnetic resonance and molecular nuclear-spin states. 
 Spin qubit quantum computer — semiconductor architecture using spin states as qubits. 
 Superconducting quantum computing — approach using superconducting electronic circuits. 
 Topological quantum computer — proposed approach using topological states such as anyons . 
 Trapped-ion quantum computer — approach using trapped charged atoms as qubits. 
 See also [ edit ] 
 D-Wave Systems   – Quantum computing company 
 Electronic quantum holography   – Information storage technology 
 Glossary of quantum computing 
 Intelligence Advanced Research Projects Activity   – American government agency 
 India's quantum computer   – Indian proposed quantum computer 
 QpiAI-Indus   – India's first full stack quantum computer 
 IonQ   – US information technology company 
 List of emerging technologies   – New technologies actively in development 
 List of quantum computing journals 
 List of quantum computing books 
 List of quantum software 
 Magic state distillation   – Quantum computing algorithm 
 Metacomputing   – Computing for the purpose of computing 
 Natural computing   – Methods that imitate, replicate or use natural processes 
 Non-local quantum computation   – Method of quantum computing via entanglement 
 Optical computing   – Computer that uses photons or light waves 
 Quantum bus   – Device to store or transfer information in quantum computing 
 Quantum cognition   – Application of quantum theory mathematics to cognitive phenomena 
 Quantum sensor   – Device measuring quantum mechanical effects 
 Quantum volume   – Metric for a quantum computer's capabilities 
 Quantum weirdness   – Unintuitive aspects of quantum mechanics 
 Rigetti Computing   – American quantum computing company 
 Supercomputer   – Type of extremely powerful computer 
 Theoretical computer science   – Subfield of computer science and mathematics 
 Unconventional computing   – Computing by new or unusual methods 
 Valleytronics   – Experimental area in semiconductors 
 Notes [ edit ] 
 ↑ The standard basis is also the computational basis . [ 39 ] 
 References [ edit ] 
 ↑ Mermin 2007 , p.   1. 
 ↑ "Quantum Computing Just Hit a Milestone That Experts Said Was a Decade Away — and the Race Is Only Getting Faster" . thefirmo . 20 May 2026 . Retrieved 23 May 2026 . 
 ↑ Aaronson 2013 , p.   132. 
 ↑ Zwiebach, Barton (2022). Mastering Quantum Mechanics: Essentials, Theory, and Applications . MIT Press. §1. ISBN   978-0-262-04613-8 . Quantum physics has replaced classical physics as the correct fundamental description of our physical universe. It is used routinely to describe most phenomena that occur at short distances. [...] The era of quantum physics began in earnest in 1925 with the discoveries of Erwin Schrödinger and Werner Heisenberg. The seeds for these discoveries were planted by Max Planck, Albert Einstein, Niels Bohr, Louis de Broglie, and others. 
 ↑ Weinberg, Steven (2015). "Historical Introduction". Lectures on Quantum Mechanics (2nd   ed.). Cambridge University Press. pp.   1– 30. ISBN   978-1-107-11166-0 . 
 ↑ Ceruzzi, Paul E. (2012). Computing: A Concise History . Cambridge, Massachusetts : MIT Press. pp.   3, 46. ISBN   978-0-262-31038-3 . OCLC   796812982 . 
 ↑ Hodges, Andrew (2014). Alan Turing: The Enigma . Princeton, New Jersey: Princeton University Press . p.   xviii. ISBN   978-0-691-16472-4 . 
 ↑ Mårtensson-Pendrill, Ann-Marie (1 November 2006). "The Manhattan project—a part of physics history". Physics Education . 41 (6): 493– 501. Bibcode : 2006PhyEd..41..493M . doi : 10.1088/0031-9120/41/6/001 . ISSN   0031-9120 . S2CID   120294023 . 
 1 2 Benioff, Paul (1980). "The computer as a physical system: A microscopic quantum mechanical Hamiltonian model of computers as represented by Turing machines". Journal of Statistical Physics . 22 (5): 563– 591. Bibcode : 1980JSP....22..563B . doi : 10.1007/bf01011339 . S2CID   122949592 . 
 ↑ Buluta, Iulia; Nori, Franco (2 October 2009). "Quantum Simulators". Science . 326 (5949): 108– 111. Bibcode : 2009Sci...326..108B . doi : 10.1126/science.1177838 . ISSN   0036-8075 . PMID   19797653 . S2CID   17187000 . 
 ↑ Manin, Yu. I. (1980). Vychislimoe i nevychislimoe [ Computable and Noncomputable ] (in Russian). Soviet Radio. pp.   13– 15. Archived from the original on 10 May 2013 . Retrieved 4 March 2013 . 
 ↑ Feynman, Richard (June 1982). "Simulating Physics with Computers" (PDF) . International Journal of Theoretical Physics . 21 (6/7): 467– 488. Bibcode : 1982IJTP...21..467F . doi : 10.1007/BF02650179 . S2CID   124545445 . Archived from the original (PDF) on 8 January 2019 . Retrieved 28 February 2019 . 
 ↑ Nielsen & Chuang 2010 , p.   214. 
 1 2 Bennett, C. H.; Brassard, G. (1984). "Quantum cryptography: Public key distribution and coin tossing". Proceedings of the International Conference on Computers, Systems & Signal Processing, Bangalore, India . Vol.   1. New York: IEEE. pp.   175– 179. Reprinted as Bennett, C. H.; Brassard, G. (4 December 2014). "Quantum cryptography: Public key distribution and coin tossing" . Theoretical Computer Science . Theoretical Aspects of Quantum Cryptography – celebrating 30 years of BB84. 560 (1): 7– 11. arXiv : 2003.06557 . Bibcode : 2014TComS.560....7B . doi : 10.1016/j.tcs.2014.05.025 . 
 ↑ Brassard, G. (2005). "Brief history of quantum cryptography: A personal perspective". IEEE Information Theory Workshop on Theory and Practice in Information-Theoretic Security, 2005 . Awaji Island, Japan: IEEE. pp.   19– 23. arXiv : quant-ph/0604072 . doi : 10.1109/ITWTPI.2005.1543949 . ISBN   978-0-7803-9491-9 . S2CID   16118245 . 
 ↑ Deutsch, D. (8 July 1985). "Quantum theory, the Church–Turing principle and the universal quantum computer". Proceedings of the Royal Society of London. A. Mathematical and Physical Sciences . 400 (1818): 97– 117. Bibcode : 1985RSPSA.400...97D . doi : 10.1098/rspa.1985.0070 . ISSN   0080-4630 . S2CID   1438116 . 
 ↑ Bernstein, Ethan; Vazirani, Umesh (1993). "Quantum complexity theory" . Proceedings of the twenty-fifth annual ACM symposium on Theory of computing – STOC '93 . San Diego, California, United States: ACM Press. pp.   11– 20. doi : 10.1145/167088.167097 . ISBN   978-0-89791-591-5 . S2CID   676378 . 
 ↑ Simon, D. R. (1994). "On the power of quantum computation". Proceedings 35th Annual Symposium on Foundations of Computer Science . Santa Fe, New Mexico, USA: IEEE Comput. Soc. Press. pp.   116– 123. doi : 10.1109/SFCS.1994.365701 . ISBN   978-0-8186-6580-6 . S2CID   7457814 . 
 1 2 Nielsen & Chuang 2010 , pp.   30–32. 
 ↑ Shor, Peter W. (1994). Algorithms for Quantum Computation: Discrete Logarithms and Factoring . Symposium on Foundations of Computer Science . Santa Fe, New Mexico : IEEE . pp.   124– 134. doi : 10.1109/SFCS.1994.365700 . ISBN   978-0-8186-6580-6 . 
 ↑ Grover, Lov K. (1996). A fast quantum mechanical algorithm for database search . ACM symposium on Theory of computing. Philadelphia : ACM Press. pp.   212– 219. arXiv : quant-ph/9605043 . doi : 10.1145/237814.237866 . ISBN   978-0-89791-785-8 . 
 1 2 Nielsen & Chuang 2010 , p.   7. 
 1 2 Lloyd, Seth (23 August 1996). "Universal Quantum Simulators". Science . 273 (5278): 1073– 1078. Bibcode : 1996Sci...273.1073L . doi : 10.1126/science.273.5278.1073 . ISSN   0036-8075 . PMID   8688088 . S2CID   43496899 . 
 ↑ Cao, Yudong; Romero, Jonathan; Olson, Jonathan P.; Degroote, Matthias; Johnson, Peter D.; et   al. (9 October 2019). "Quantum Chemistry in the Age of Quantum Computing". Chemical Reviews . 119 (19): 10856– 10915. arXiv : 1812.09976 . Bibcode : 2019ChRv..11910856C . doi : 10.1021/acs.chemrev.8b00803 . ISSN   0009-2665 . PMID   31469277 . S2CID   119417908 . 
 1 2 Grumbling & Horowitz 2019 , pp.   164–169. 
 ↑ Chuang, Isaac L.; Gershenfeld, Neil; Kubinec, Markdoi (April 1998). "Experimental Implementation of Fast Quantum Searching". Physical Review Letters . 80 (15). American Physical Society : 3408– 3411. Bibcode : 1998PhRvL..80.3408C . doi : 10.1103/PhysRevLett.80.3408 . 
 ↑ Holton, William Coffeen. "quantum computer" . Encyclopedia Britannica . Encyclopædia Britannica . Retrieved 4 December 2021 . 
 ↑ Gibney, Elizabeth (23 October 2019). "Hello quantum world! Google publishes landmark quantum supremacy claim" . Nature . 574 (7779): 461– 462. Bibcode : 2019Natur.574..461G . doi : 10.1038/d41586-019-03213-z . PMID   31645740 . 
 1 2 Lay summary: Martinis, John; Boixo, Sergio (23 October 2019). "Quantum Supremacy Using a Programmable Superconducting Processor" . Nature . 574 (7779). Google AI : 505– 510. arXiv : 1910.11333 . Bibcode : 2019Natur.574..505A . doi : 10.1038/s41586-019-1666-5 . PMID   31645734 . S2CID   204836822 . Retrieved 27 April 2022 .   • Journal article: Arute, Frank; Arya, Kunal; Babbush, Ryan; Bacon, Dave; Bardin, Joseph C.; et   al. (23 October 2019). "Quantum supremacy using a programmable superconducting processor". Nature . 574 (7779): 505– 510. arXiv : 1910.11333 . Bibcode : 2019Natur.574..505A . doi : 10.1038/s41586-019-1666-5 . PMID   31645734 . S2CID   204836822 . 
 ↑ Aaronson, Scott (30 October 2019). "Opinion | Why Google's Quantum Supremacy Milestone Matters" . The New York Times . ISSN   0362-4331 . Retrieved 25 September 2021 . 
 ↑ Pan, Feng; Zhang, Pan (4 March 2021). "Simulating the Sycamore quantum supremacy circuits". arXiv : 2103.03074 [ quant-ph ]. 
 ↑ Sample, Ian (23 October 2019). "Google claims it has achieved 'quantum supremacy' – but IBM disagrees" . The Guardian . ISSN   0261-3077 . Retrieved 1 August 2025 . 
 ↑ Bravyi (2024). "High-threshold and low-overhead fault-tolerant quantum memory" . Nature . 627 (8005): 778– 782. arXiv : 2308.07915 . Bibcode : 2024Natur.627..778B . doi : 10.1038/s41586-024-07107-7 . PMC   10972743 . PMID   38538939 . 
 ↑ Fredkin, Edward ; Toffoli, Tommaso (1982). "Conservative logic". International Journal of Theoretical Physics . 21 ( 3– 4): 219– 253. Bibcode : 1982IJTP...21..219F . doi : 10.1007/BF01857727 . 
 ↑ Arora, Sanjeev ; Barak, Boaz (2009). Computational Complexity: A Modern Approach . Cambridge University Press. pp.   123– 125. 
 ↑ Bennett, Charlie (31 July 2020). Information Is Quantum: How Physics Helped Explain the Nature of Information and What Can Be Done With It (Videotape). Event occurs at 1:08:22 – via YouTube. 
 ↑ Nielsen & Chuang 2010 , p.   13. 
 1 2 Mermin 2007 , p.   17. 
 1 2 Mermin 2007 , p.   18. 
 ↑ Aaronson 2013 , p.   110. 
 ↑ Mermin 2007 , pp.   38–39. 
 ↑ Kurgalin, Sergei; Borzunov, Sergei (2021). Concise guide to quantum computing: algorithms, exercises, and implementations . Texts in computer science. Cham: Springer. ISBN   978-3-030-65054-4 . 
 1 2 Chi-Chih Yao, A. (1993). "Quantum circuit complexity". Proceedings of 1993 IEEE 34th Annual Foundations of Computer Science . pp.   352– 361. doi : 10.1109/SFCS.1993.366852 . ISBN   0-8186-4370-6 . S2CID   195866146 . 
 ↑ Raussendorf, Robert; Browne, Daniel E.; Briegel, Hans J. (25 August 2003). "Measurement-based quantum computation on cluster states". Physical Review A . 68 (2) 022312. arXiv : quant-ph/0301052 . Bibcode : 2003PhRvA..68b2312R . doi : 10.1103/PhysRevA.68.022312 . S2CID   6197709 . 
 ↑ Aharonov, Dorit; van Dam, Wim; Kempe, Julia; Landau, Zeph; Lloyd, Seth; Regev, Oded (1 January 2008). "Adiabatic Quantum Computation Is Equivalent to Standard Quantum Computation". SIAM Review . 50 (4): 755– 787. arXiv : quant-ph/0405098 . Bibcode : 2008SIAMR..50..755A . doi : 10.1137/080734479 . ISSN   0036-1445 . S2CID   1503123 . 
 ↑ Freedman, Michael H.; Larsen, Michael; Wang, Zhenghan (1 June 2002). "A Modular Functor Which is Universal for Quantum Computation". Communications in Mathematical Physics . 227 (3): 605– 622. arXiv : quant-ph/0001108 . Bibcode : 2002CMaPh.227..605F . doi : 10.1007/s002200200645 . ISSN   0010-3616 . S2CID   8990600 . 
 ↑ Das, A.; Chakrabarti, B. K. (2008). "Quantum Annealing and Analog Quantum Computation". Rev. Mod. Phys. 80 (3): 1061– 1081. arXiv : 0801.2193 . Bibcode : 2008RvMP...80.1061D . doi : 10.1103/RevModPhys.80.1061 . S2CID   14255125 . 
 ↑ Nayak, Chetan; Simon, Steven; Stern, Ady; Das Sarma, Sankar (2008). "Nonabelian Anyons and Quantum Computation". Reviews of Modern Physics . 80 (3): 1083– 1159. arXiv : 0707.1889 . Bibcode : 2008RvMP...80.1083N . doi : 10.1103/RevModPhys.80.1083 . S2CID   119628297 . 
 ↑ Nielsen & Chuang 2010 , p.   481. 
 1 2 3 4 5 Preskill, John (6 August 2018). "Quantum Computing in the NISQ era and beyond" . Quantum . 2 79. arXiv : 1801.00862 . Bibcode : 2018Quant...2...79P . doi : 10.22331/q-2018-08-06-79 . S2CID   44098998 . 
 ↑ Bluvstein, Dolev; Evered, Simon J.; Geim, Alexandra A.; Li, Sophie H.; Zhou, Hengyun; Manovitz, Tom; Ebadi, Sepehr; Cain, Madelyn; Kalinowski, Marcin; Hangleiter, Dominik; Ataides, J. Pablo Bonilla; Maskara, Nishad; Cong, Iris; Gao, Xun; Rodriguez, Pedro Sales (6 December 2023). "Logical quantum processor based on reconfigurable atom arrays" . Nature . 626 (7997): 58– 65. arXiv : 2312.03982 . doi : 10.1038/s41586-023-06927-3 . ISSN   1476-4687 . PMC   10830422 . PMID   38056497 . S2CID   266052773 . 
 ↑ "DARPA-Funded Research Leads to Quantum Computing Breakthrough" . darpa.mil . 6 December 2023 . Retrieved 5 January 2024 . 
 ↑ Choudhury, Rizwan (30 December 2023). "Top 7 innovation stories of 2023 – Interesting Engineering" . interestingengineering.com . Retrieved 6 January 2024 . 
 ↑ Pirandola, S.; Andersen, U. L.; Banchi, L.; Berta, M.; Bunandar, D.; Colbeck, R.; Englund, D.; Gehring, T.; Lupo, C.; Ottaviani, C.; Pereira, J.; Razavi, M.; Shamsul Shaari, J.; Tomamichel, M.; Usenko, V. C.; Vallone, G.; Villoresi, P.; Wallden, P. (2020). "Advances in quantum cryptography". Advances in Optics and Photonics . 12 (4): 1012. arXiv : 1906.01645 . Bibcode : 2020AdOP...12.1012P . doi : 10.1364/AOP.361502 . 
 ↑ Xu, Feihu; Ma, Xiongfeng; Zhang, Qiang; Lo, Hoi-Kwong; Pan, Jian-Wei (26 May 2020). "Secure quantum key distribution with realistic devices". Reviews of Modern Physics . 92 (2): 025002 - 3. arXiv : 1903.09051 . Bibcode : 2020RvMP...92b5002X . doi : 10.1103/RevModPhys.92.025002 . S2CID   210942877 . 
 ↑ Xu, Guobin; Mao, Jianzhou; Sakk, Eric; Wang, Shuangbao Paul (22 March 2023). "An Overview of Quantum-Safe Approaches: Quantum Key Distribution and Post-Quantum Cryptography". 2023 57th Annual Conference on Information Sciences and Systems (CISS) . IEEE . p.   3. doi : 10.1109/CISS56502.2023.10089619 . ISBN   978-1-6654-5181-9 . 
 ↑ Kozlowski, Wojciech; Wehner, Stephanie (25 September 2019). "Towards Large-Scale Quantum Networks". Proceedings of the Sixth Annual ACM International Conference on Nanoscale Computing and Communication . ACM. pp.   1– 7. arXiv : 1909.08396 . doi : 10.1145/3345312.3345497 . ISBN   978-1-4503-6897-1 . 
 ↑ Guo, Xueshi; Breum, Casper R.; Borregaard, Johannes; Izumi, Shuro; Larsen, Mikkel V.; Gehring, Tobias; Christandl, Matthias; Neergaard-Nielsen, Jonas S.; Andersen, Ulrik L. (23 December 2019). "Distributed quantum sensing in a continuous-variable entangled network". Nature Physics . 16 (3): 281– 284. arXiv : 1905.09408 . doi : 10.1038/s41567-019-0743-x . ISSN   1745-2473 . S2CID   256703226 . 
 1 2 3 Jordan, Stephen (14 October 2022) [22 April 2011]. "Quantum Algorithm Zoo" . Archived from the original on 29 April 2018. 
 ↑ Aaronson, Scott ; Arkhipov, Alex (6 June 2011). "The computational complexity of linear optics". Proceedings of the forty-third annual ACM symposium on Theory of computing . San Jose, California : Association for Computing Machinery . pp.   333– 342. arXiv : 1011.3245 . doi : 10.1145/1993636.1993682 . ISBN   978-1-4503-0691-1 . 
 1 2 Nielsen & Chuang 2010 , p.   42. 
 ↑ Preskill 2018 . 
 ↑ Norton, Quinn (15 February 2007). "The Father of Quantum Computing" . Wired . 
 ↑ Weidman, Jared D.; Sajjan, Manas; Mikolas, Camille; Stewart, Zachary J.; Pollanen, Johannes; Kais, Sabre; Wilson, Angela K. (18 September 2024). "Quantum computing and chemistry" . Cell Reports Physical Science . 5 (9) 102105. Bibcode : 2024CRPS....502105W . doi : 10.1016/j.xcrp.2024.102105 . 
 ↑ Ambainis, Andris (Spring 2014). "What Can We Do with a Quantum Computer?" . Institute for Advanced Study. 
 ↑ Chang, Kenneth (14 June 2023). "Quantum Computing Advance Begins New Era, IBM Says – A quantum computer came up with better answers to a physics problem than a conventional supercomputer" . The New York Times . Retrieved 15 June 2023 . {{ cite news }} : CS1 maint: deprecated archival service ( link ) 
 ↑ Kim, Youngseok; et   al. (14 June 2023). "Evidence for the utility of quantum computing before fault tolerance" . Nature . 618 (7965): 500– 505. Bibcode : 2023Natur.618..500K . doi : 10.1038/s41586-023-06096-3 . PMC   10266970 . PMID   37316724 . 
 ↑ Morello, Andrea (21 November 2018). Lunch & Learn: Quantum Computing . Sibos TV . Archived from the original on 15 February 2021 . Retrieved 4 February 2021 – via YouTube. {{ cite AV media }} : CS1 maint: bot: original URL status unknown ( link ) 
 ↑ Ruane, Jonathan; McAfee, Andrew; Oliver, William D. (1 January 2022). "Quantum Computing for Business Leaders" . Harvard Business Review . ISSN   0017-8012 . Retrieved 12 April 2023 . 
 ↑ Budde, Florian; Volz, Daniel (12 July 2019). "Quantum computing and the chemical industry | McKinsey" . www.mckinsey.com . McKinsey and Company . Retrieved 12 April 2023 . [ dead link ] 
 ↑ Bourzac, Katherine (30 October 2017). "Chemistry is quantum computing's killer app" . cen.acs.org . American Chemical Society . Retrieved 12 April 2023 . 
 1 2 Gisin, Nicolas; Ribordy, Grégoire; Tittel, Wolfgang; Zbinden, Hugo (8 March 2002). "Quantum cryptography" . Reviews of Modern Physics . 74 (1): 145– 195. arXiv : quant-ph/0101098 . Bibcode : 2002RvMP...74..145G . doi : 10.1103/RevModPhys.74.145 . ISSN   0034-6861 . 
 ↑ Lenstra, Arjen K. (2000). "Integer Factoring" (PDF) . Designs, Codes and Cryptography . 19 (2/3): 101– 128. doi : 10.1023/A:1008397921377 . S2CID   9816153 . Archived from the original (PDF) on 10 April 2015. 
 ↑ Nielsen & Chuang 2010 , p.   216. 
 1 2 Bernstein, Daniel J. (2009). "Introduction to post-quantum cryptography". Post-Quantum Cryptography . Berlin, Heidelberg: Springer. pp.   1– 14. doi : 10.1007/978-3-540-88702-7_1 . ISBN   978-3-540-88701-0 . S2CID   61401925 . 
 ↑ See also pqcrypto.org , a bibliography maintained by Daniel J. Bernstein and Tanja Lange on cryptography not known to be broken by quantum computing. 
 ↑ McEliece, R. J. (January 1978). "A Public-Key Cryptosystem Based On Algebraic Coding Theory" (PDF) . DSNPR . 44 : 114– 116. Bibcode : 1978DSNPR..44..114M . 
 ↑ Kobayashi, H.; Gall, F. L. (2006). "Dihedral Hidden Subgroup Problem: A Survey" . Information and Media Technologies . 1 (1): 178– 185. doi : 10.2197/ipsjdc.1.470 . 
 ↑ Bennett, Charles H.; Bernstein, Ethan; Brassard, Gilles; Vazirani, Umesh (October 1997). "Strengths and Weaknesses of Quantum Computing". SIAM Journal on Computing . 26 (5): 1510– 1523. arXiv : quant-ph/9701001 . Bibcode : 1997quant.ph..1001B . doi : 10.1137/s0097539796300933 . S2CID   13403194 . 
 ↑ Brassard, Gilles; Høyer, Peter; Tapp, Alain (2016). "Quantum Algorithm for the Collision Problem". In Kao, Ming-Yang (ed.). Encyclopedia of Algorithms . New York, New York: Springer. pp.   1662– 1664. arXiv : quant-ph/9705002 . doi : 10.1007/978-1-4939-2864-4_304 . ISBN   978-1-4939-2864-4 . S2CID   3116149 . 
 ↑ Farhi, Edward; Goldstone, Jeffrey; Gutmann, Sam (23 December 2008). "A Quantum Algorithm for the Hamiltonian NAND Tree" . Theory of Computing . 4 (1): 169– 190. doi : 10.4086/toc.2008.v004a008 . ISSN   1557-2862 . S2CID   8258191 . 
 ↑ Williams, Colin P. (2011). Explorations in Quantum Computing . Springer . pp.   242– 244. ISBN   978-1-84628-887-6 . 
 ↑ Grover, Lov (29 May 1996). "A fast quantum mechanical algorithm for database search". arXiv : quant-ph/9605043 . 
 ↑ Ambainis, Ambainis (June 2004). "Quantum search algorithms". ACM SIGACT News . 35 (2): 22– 35. arXiv : quant-ph/0504012 . Bibcode : 2005quant.ph..4012A . doi : 10.1145/992287.992296 . S2CID   11326499 . 
 ↑ Rich, Steven; Gellman, Barton (1 February 2014). "NSA seeks to build quantum computer that could crack most types of encryption" . The Washington Post . 
 ↑ Lucas, Andrew (2014). "Ising formulations of many NP problems" . Frontiers in Physics . 2 : 5. arXiv : 1302.5843 . Bibcode : 2014FrP.....2....5L . doi : 10.3389/fphy.2014.00005 . 
 ↑ Outeiral, Carlos; Strahm, Martin; Morris, Garrett; Benjamin, Simon; Deane, Charlotte; Shi, Jiye (2021). "The prospects of quantum computing in computational molecular biology" . WIREs Computational Molecular Science . 11 e1481. arXiv : 2005.12792 . doi : 10.1002/wcms.1481 . S2CID   218889377 . 
 ↑ Biamonte, Jacob; Wittek, Peter; Pancotti, Nicola; Rebentrost, Patrick; Wiebe, Nathan; Lloyd, Seth (September 2017). "Quantum machine learning". Nature . 549 (7671): 195– 202. arXiv : 1611.09347 . Bibcode : 2017Natur.549..195B . doi : 10.1038/nature23474 . ISSN   0028-0836 . PMID   28905917 . S2CID   64536201 . 
 ↑ Wang, Yuxuan; Xue, Zhaohui; Yuan, Jie; Zhao, Yijia; Li, Yuan; Wu, Yonghao; Pan, Jian-Wei (2024). "A comprehensive review of quantum machine learning" . Fundamental Research . 5 (2): 378– 417. doi : 10.1016/j.fmre.2024.01.008 . PMC   12869772 . PMID   41647569 . 
 ↑ Jerbi, Sofiene; Gyurik, Casper; Marshall, Simon C.; Molteni, Riccardo; Dunjko, Vedran (6 July 2024). "Shadows of quantum machine learning" . Nature Communications . 15 (1) 5676. arXiv : 2306.00061 . Bibcode : 2024NatCo..15.5676J . doi : 10.1038/s41467-024-49877-8 . hdl : 1887/4170178 . PMC   11227511 . PMID   38971826 . 
 ↑ Harrow, Aram; Hassidim, Avinatan; Lloyd, Seth (2009). "Quantum algorithm for solving linear systems of equations". Physical Review Letters . 103 (15) 150502. arXiv : 0811.3171 . Bibcode : 2009PhRvL.103o0502H . doi : 10.1103/PhysRevLett.103.150502 . PMID   19905613 . S2CID   5187993 . 
 ↑ Benedetti, Marcello; Realpe-Gómez, John; Biswas, Rupak; Perdomo-Ortiz, Alejandro (9 August 2016). "Estimation of effective temperatures in quantum annealers for sampling applications: A case study with possible applications in deep learning" . Physical Review A . 94 (2) 022308. arXiv : 1510.07611 . Bibcode : 2016PhRvA..94b2308B . doi : 10.1103/PhysRevA.94.022308 . 
 ↑ Ajagekar, Akshay; You, Fengqi (5 December 2020). "Quantum computing assisted deep learning for fault detection and diagnosis in industrial process systems". Computers & Chemical Engineering . 143 107119. arXiv : 2003.00264 . doi : 10.1016/j.compchemeng.2020.107119 . ISSN   0098-1354 . S2CID   211678230 . 
 ↑ Ajagekar, Akshay; You, Fengqi (1 December 2021). "Quantum computing based hybrid deep learning for fault diagnosis in electrical power systems" . Applied Energy . 303 117628. Bibcode : 2021ApEn..30317628A . doi : 10.1016/j.apenergy.2021.117628 . ISSN   0306-2619 . 
 ↑ Fedichev, Peter ; Pyrkov, Timothy; Krylov, Ivan (2023). "Quantum machine learning for drug discovery" . Scientific Reports . 13 (1): 8250. doi : 10.1038/s41598-023-32703-4 . PMC   10201520 . PMID   37217521 . 
 ↑ Borfitz, Deborah (22 August 2023). "Gero Taps Quantum Computing and AI To Tackle Diseases Of Aging" . Bio-IT World . 
 ↑ Gao, Xun; Anschuetz, Eric R.; Wang, Sheng-Tao; Cirac, J. Ignacio; Lukin, Mikhail D. (2022). "Enhancing Generative Models via Quantum Correlations". Physical Review X . 12 (2) 021037. arXiv : 2101.08354 . Bibcode : 2022PhRvX..12b1037G . doi : 10.1103/PhysRevX.12.021037 . S2CID   231662294 . 
 ↑ Li, Junde; Topaloglu, Rasit; Ghosh, Swaroop (9 January 2021). "Quantum Generative Models for Small Molecule Drug Discovery". IEEE Transactions on Quantum Engineering . 2 : 1– 8. arXiv : 2101.03438 . Bibcode : 2021ITQE....2E4804L . doi : 10.1109/TQE.2021.3104804 . 
 ↑ Novikov, Alexander; et   al. (16 June 2025). "AlphaEvolve: A coding agent for scientific and algorithmic discovery". arXiv : 2506.13131 [ cs.AI ]. 
 ↑ Zhang, C.; et   al. (22 October 2025). "Quantum computation of molecular geometry via many-body nuclear spin echoes". arXiv : 2510.19550 [ quant-ph ]. 
 1 2 3 
 Brooks, Michael (24 May 2023). "Quantum computers: what are they good for?" . Nature . 617 (7962): S1– S3. Bibcode : 2023Natur.617S...1B . doi : 10.1038/d41586-023-01692-9 . PMID   37225885 . S2CID   258847001 . 
 1 2 3 4 Torsten Hoefler; Thomas Häner; Matthias Troyer (May 2023). "Disentangling Hype from Practicality: On Realistically Achieving Quantum Advantage" . Communications of the ACM. 
 ↑ Dyakonov, Mikhail (15 November 2018). "The Case Against Quantum Computing" . IEEE Spectrum . 
 ↑ 3.0.CO;2-E"},"volume":{"wt":"48"},"issue":{"wt":"9–11"},"journal":{"wt":"Fortschritte der Physik"},"pages":{"wt":"771–783"},"bibcode":{"wt":"2000ForPh..48..771D"},"s2cid":{"wt":"15439711"}},"i":0}}]}'/> DiVincenzo, David P. (13 April 2000). "The Physical Implementation of Quantum Computation". Fortschritte der Physik . 48 ( 9– 11): 771– 783. arXiv : quant-ph/0002077 . Bibcode : 2000ForPh..48..771D . doi : 10.1002/1521-3978(200009)48:9/11 < 771::AID-PROP771 > 3.0.CO ; 2-E . S2CID   15439711 . 
 ↑ Pauka SJ, Das K, Kalra B, Moini A, Yang Y, Trainer M, Bousquet A, Cantaloube C, Dick N, Gardner GC, Manfra MJ, Reilly DJ (2021). "A cryogenic CMOS chip for generating control signals for multiple qubits" . Nature Electronics . 4 (4): 64– 70. arXiv : 1912.01299 . doi : 10.1038/s41928-020-00528-y . S2CID   231715555 . 
 ↑ "Post-Quantum Cryptography Standardization" . NIST (National Institute of Standards and Technology) . 3 January 2017. 
 ↑ Giles, Martin (17 January 2019). "We'd have more quantum computers if it weren't so hard to find the damn cables" . MIT Technology Review . Retrieved 17 May 2021 . 
 ↑ "A rare earth 'China solution' that leaves US defence agency in the cold" . South China Morning Post . 17 March 2026 . Retrieved 14 April 2026 . 
 ↑ DiVincenzo, David P. (1995). "Quantum Computation". Science . 270 (5234): 255– 261. Bibcode : 1995Sci...270..255D . doi : 10.1126/science.270.5234.255 . S2CID   220110562 . 
 ↑ Zu, H.; Dai, W.; de Waele, A.T.A.M. (2022). "Development of Dilution refrigerators – A review". Cryogenics . 121 . doi : 10.1016/j.cryogenics.2021.103390 . ISSN   0011-2275 . S2CID   244005391 . 
 ↑ Jones, Nicola (19 June 2013). "Computing: The quantum company" . Nature . 498 (7454): 286– 288. Bibcode : 2013Natur.498..286J . doi : 10.1038/498286a . PMID   23783610 . 
 ↑ Vepsäläinen, Antti P.; Karamlou, Amir H.; Orrell, John L.; Dogra, Akshunna S.; Loer, Ben; et   al. (August 2020). "Impact of ionizing radiation on superconducting qubit coherence" . Nature . 584 (7822): 551– 556. arXiv : 2001.09190 . Bibcode : 2020Natur.584..551V . doi : 10.1038/s41586-020-2619-8 . ISSN   1476-4687 . PMID   32848227 . S2CID   210920566 . 
 ↑ Amy, Matthew; Matteo, Olivia; Gheorghiu, Vlad; Mosca, Michele; Parent, Alex; Schanck, John (30 November 2016). "Estimating the cost of generic quantum pre-image attacks on SHA-2 and SHA-3". arXiv : 1603.09383 [ quant-ph ]. 
 ↑ Dyakonov, M. I. (14 October 2006). S. Luryi; Xu, J.; Zaslavsky, A. (eds.). "Is Fault-Tolerant Quantum Computation Really Possible?". Future Trends in Microelectronics. Up the Nano Creek : 4– 18. arXiv : quant-ph/0610117 . Bibcode : 2006quant.ph.10117D . 
 ↑ Ahsan, Muhammad (2015). Architecture Framework for Trapped-ion Quantum Computer based on Performance Simulation Tool . Bibcode : 2015PhDT........56A . OCLC   923881411 . 
 ↑ Ahsan, Muhammad; Meter, Rodney Van; Kim, Jungsang (28 December 2016). "Designing a Million-Qubit Quantum Computer Using a Resource Performance Simulator" . ACM Journal on Emerging Technologies in Computing Systems . 12 (4): 39:1–39:25. arXiv : 1512.00796 . doi : 10.1145/2830570 . ISSN   1550-4832 . S2CID   1258374 . 
 ↑ Gidney, Craig; Ekerå, Martin (15 April 2021). "How to factor 2048 bit RSA integers in 8 hours using 20 million noisy qubits". Quantum . 5 433. arXiv : 1905.09749 . Bibcode : 2021Quant...5..433G . doi : 10.22331/q-2021-04-15-433 . ISSN   2521-327X . S2CID   162183806 . 
 ↑ Ruiz, Diego; Guillaud, Jérémie; Leverrier, Anthony; Mirrahimi, Mazyar; Vuillot, Christophe (26 January 2025). "LDPC-cat codes for low-overhead quantum computing in 2D" . Nature Communications . 16 (1) 1040. arXiv : 2401.09541 . Bibcode : 2025NatCo..16.1040R . doi : 10.1038/s41467-025-56298-8 . ISSN   2041-1723 . PMC   11762751 . PMID   39863608 . 
 ↑ Freedman, Michael H. ; Kitaev, Alexei ; Larsen, Michael J. ; Wang, Zhenghan (2003). "Topological quantum computation". Bulletin of the American Mathematical Society . 40 (1): 31– 38. arXiv : quant-ph/0101025 . doi : 10.1090/S0273-0979-02-00964-3 . MR   1943131 . 
 ↑ Monroe, Don (1 October 2008). "Anyons: The breakthrough quantum computing needs?" . New Scientist . 
 1 2 Cossins, Daniel (28 June 2025). "How to think about...Quasiparticles". New Scientist . 266 (3549): 34. doi : 10.1016/S0262-4079(25)01046-2 . 
 1 2 Main, D.; Drmota, P.; Nadlinger, D. P.; Ainley, E. M.; Agrawal, A.; Nichol, B. C.; Srinivas, R.; Araneda, G.; Lucas, D. M. (2025). "Distributed quantum computing across an optical network link" . Nature . 638 (8050): 383– 388. arXiv : 2407.00835 . Bibcode : 2025Natur.638..383M . doi : 10.1038/s41586-024-08404-x . PMC   11821536 . PMID   39910308 . 
 ↑ "First distributed quantum algorithm brings quantum supercomputers closer" . University of Oxford. 6 February 2025 . Retrieved 2 July 2026 . 
 ↑ Iuliano, M.; et   al. (2026). "Unconditionally teleported quantum gates between remote solid-state qubit registers" . Nature Communications . 17 (1) 4694. arXiv : 2601.04848 . Bibcode : 2026NatCo..17.4694I . doi : 10.1038/s41467-026-72818-6 . PMC   13212882 . PMID   42191685 . 
 ↑ Knörzer, J; Liu, X; Schiffer, B F; Tura, J (1 July 2026). "Distributed quantum information processing: a review of recent progress". Reports on Progress in Physics . 89 (7): 074401. arXiv : 2510.15630 . doi : 10.1088/1361-6633/ae74e0 . ISSN   0034-4885 . PMID   42214383 . 
 ↑ Preskill, John (26 March 2012). "Quantum computing and the entanglement frontier". arXiv : 1203.5813 [ quant-ph ]. 
 ↑ Boixo, Sergio; Isakov, Sergei V.; Smelyanskiy, Vadim N.; Babbush, Ryan; Ding, Nan; et   al. (2018). "Characterizing Quantum Supremacy in Near-Term Devices". Nature Physics . 14 (6): 595– 600. arXiv : 1608.00263 . Bibcode : 2018NatPh..14..595B . doi : 10.1038/s41567-018-0124-x . S2CID   4167494 . 
 ↑ Savage, Neil (5 July 2017). "Quantum Computers Compete for "Supremacy" " . Scientific American . 
 ↑ Giles, Martin (20 September 2019). "Google researchers have reportedly achieved 'quantum supremacy' " . MIT Technology Review . Retrieved 15 May 2020 . 
 ↑ Tavares, Frank (23 October 2019). "Google and NASA Achieve Quantum Supremacy" . NASA . Retrieved 16 November 2021 . 
 ↑ Pednault, Edwin; Gunnels, John A.; Nannicini, Giacomo; Horesh, Lior; Wisnieff, Robert (22 October 2019). "Leveraging Secondary Storage to Simulate Deep 54-qubit Sycamore Circuits". arXiv : 1910.09534 [ quant-ph ]. 
 ↑ Cho, Adrian (23 October 2019). "IBM casts doubt on Google's claims of quantum supremacy" . Science . doi : 10.1126/science.aaz6080 . ISSN   0036-8075 . S2CID   211982610 . 
 ↑ Liu, Yong (Alexander); Liu, Xin (Lucy); Li, Fang (Nancy); Fu, Haohuan; Yang, Yuling; et   al. (14 November 2021). "Closing the "quantum supremacy" gap". Proceedings of the International Conference for High Performance Computing, Networking, Storage and Analysis . SC '21. New York, New York: Association for Computing Machinery. pp.   1– 12. arXiv : 2110.14502 . doi : 10.1145/3458817.3487399 . ISBN   978-1-4503-8442-1 . S2CID   239036985 . 
 ↑ Bulmer, Jacob F. F.; Bell, Bryn A.; Chadwick, Rachel S.; Jones, Alex E.; Moise, Diana; et   al. (28 January 2022). "The boundary for quantum advantage in Gaussian boson sampling" . Science Advances . 8 (4) eabl9236. arXiv : 2108.01622 . Bibcode : 2022SciA....8.9236B . doi : 10.1126/sciadv.abl9236 . ISSN   2375-2548 . PMC   8791606 . PMID   35080972 . 
 ↑ McCormick, Katie (10 February 2022). "Race Not Over Between Classical and Quantum Computers" . Physics . 15 19. Bibcode : 2022PhyOJ..15...19M . doi : 10.1103/Physics.15.19 . S2CID   246910085 . 
 ↑ Pan, Feng; Chen, Keyang; Zhang, Pan (2022). "Solving the Sampling Problem of the Sycamore Quantum Circuits". Physical Review Letters . 129 (9) 090502. arXiv : 2111.03011 . Bibcode : 2022PhRvL.129i0502P . doi : 10.1103/PhysRevLett.129.090502 . PMID   36083655 . S2CID   251755796 . 
 ↑ Cho, Adrian (2 August 2022). "Ordinary computers can beat Google's quantum computer after all" . Science . 377 . doi : 10.1126/science.ade2364 . 
 ↑ "Google's 'quantum supremacy' usurped by researchers using ordinary supercomputer" . TechCrunch . 5 August 2022 . Retrieved 7 August 2022 . 
 ↑ Ball, Philip (3 December 2020). "Physicists in China challenge Google's 'quantum advantage' ". Nature . 588 (7838): 380. Bibcode : 2020Natur.588..380B . doi : 10.1038/d41586-020-03434-7 . PMID   33273711 . S2CID   227282052 . 
 ↑ Garisto, Daniel. "Light-based Quantum Computer Exceeds Fastest Classical Supercomputers" . Scientific American . Retrieved 7 December 2020 . 
 ↑ Conover, Emily (3 December 2020). "The new light-based quantum computer Jiuzhang has achieved quantum supremacy" . Science News . Retrieved 7 December 2020 . 
 ↑ Zhong, Han-Sen; Wang, Hui; Deng, Yu-Hao; Chen, Ming-Cheng; Peng, Li-Chao; et   al. (3 December 2020). "Quantum computational advantage using photons". Science . 370 (6523): 1460– 1463. arXiv : 2012.01625 . Bibcode : 2020Sci...370.1460Z . doi : 10.1126/science.abe8770 . ISSN   0036-8075 . PMID   33273064 . S2CID   227254333 . 
 ↑ Roberson, Tara M. (21 May 2020). "Can Hype Be a Force for Good?" . Public Understanding of Science . 29 (5): 544– 552. doi : 10.1177/0963662520923109 . ISSN   0963-6625 . PMID   32438851 . S2CID   218831653 . 
 ↑ Cavaliere, Fabio; Mattsson, John; Smeets, Ben (September 2020). "The security implications of quantum cryptography and quantum computing" . Network Security . 2020 (9): 9– 15. doi : 10.1016/S1353-4858(20)30105-7 . ISSN   1353-4858 . S2CID   222349414 . 
 ↑ Liu, Yong; Chen, Yaojian; Guo, Chu; Song, Jiawei; Shi, Xinmin; Gan, Lin; Wu, Wenzhao; Wu, Wei; Fu, Haohuan; Liu, Xin; Chen, Dexun; Zhao, Zhifeng; Yang, Guangwen; Gao, Jiangang (16 January 2024). "Verifying Quantum Advantage Experiments with Multiple Amplitude Tensor Network Contraction" . Physical Review Letters . 132 (3) 030601. arXiv : 2212.04749 . Bibcode : 2024PhRvL.132c0601L . doi : 10.1103/PhysRevLett.132.030601 . ISSN   0031-9007 . PMID   38307065 . 
 ↑ Monroe, Don (December 2022). "Quantum Computers and the Universe" . Communications of the ACM. 
 ↑ Swayne, Matt (20 June 2023). "PsiQuantum Sees 700x Reduction in Computational Resource Requirements to Break Elliptic Curve Cryptography With a Fault Tolerant Quantum Computer" . The Quanrum Insider . 
 ↑ Unruh, Bill (1995). "Maintaining coherence in Quantum Computers". Physical Review A . 51 (2): 992– 997. arXiv : hep-th/9406058 . Bibcode : 1995PhRvA..51..992U . doi : 10.1103/PhysRevA.51.992 . PMID   9911677 . S2CID   13980886 . 
 ↑ Davies, Paul (6 March 2007). "The implications of a holographic universe for quantum information science and the nature of physical law". arXiv : quant-ph/0703041 . 
 ↑ Regan, K. W. (23 April 2016). "Quantum Supremacy and Complexity" . Gödel's Lost Letter and P=NP . 
 ↑ Kalai, Gil (May 2016). "The Quantum Computer Puzzle" (PDF) . Notices of the AMS . 63 (5): 508– 516. 
 ↑ Rinott, Yosef; Shoham, Tomer; Kalai, Gil (13 July 2021). "Statistical Aspects of the Quantum Supremacy Demonstration". arXiv : 2008.05177 [ quant-ph ]. 
 ↑ Dyakonov, Mikhail (15 November 2018). "The Case Against Quantum Computing" . IEEE Spectrum . Retrieved 3 December 2019 . 
 ↑ Russell, John (10 January 2019). "IBM Quantum Update: Q System One Launch, New Collaborators, and QC Center Plans" . HPCwire . Retrieved 9 January 2023 . 
 ↑ Tacchino, Francesco; Chiesa, Alessandro; Carretta, Stefano; Gerace, Dario (19 December 2019). "Quantum Computers as Universal Quantum Simulators: State-of-the-Art and Perspectives" . Advanced Quantum Technologies . 3 (3) 1900052. arXiv : 1907.03505 . doi : 10.1002/qute.201900052 . ISSN   2511-9044 . S2CID   195833616 . 
 ↑ Grumbling & Horowitz 2019 , p.   127. 
 ↑ Grumbling & Horowitz 2019 , p.   114. 
 ↑ Nayak, Chetan; Simon, Steven H.; Stern, Ady; Freedman, Michael; Das Sarma, Sankar (12 September 2008). "Non-Abelian anyons and topological quantum computation" . Reviews of Modern Physics . 80 (3): 1083– 1159. arXiv : 0707.1889 . Bibcode : 2008RvMP...80.1083N . doi : 10.1103/RevModPhys.80.1083 . 
 ↑ Grumbling & Horowitz 2019 , p.   119. 
 ↑ Grumbling & Horowitz 2019 , p.   126. 
 1 2 "Scientists achieve breakthrough on quantum signaling" . Stanford Report . Stanford University. 1 December 2025 . Retrieved 8 January 2026 . 
 ↑ Pan, F.; Liu, F.; Heinz, T. F.; Dionne, J. A. (2025). "Room-temperature spin–photon interface in a molybdenum diselenide–silicon nanostructured device". Nature Communications . 
 ↑ "Room-Temperature Device Advances Quantum Communication" . Quantum Zeitgeist . 2 December 2025 . Retrieved 8 January 2026 . 
 ↑ Nielsen & Chuang 2010 , p.   29. 
 ↑ Nielsen & Chuang 2010 , p.   126. 
 ↑ Nielsen & Chuang 2010 , p.   41. 
 ↑ Nielsen & Chuang 2010 , p.   201. 
 ↑ Bernstein, Ethan; Vazirani, Umesh (1997). "Quantum Complexity Theory" . SIAM Journal on Computing . 26 (5): 1411– 1473. doi : 10.1137/S0097539796300921 . 
 ↑ "Hanyuan No. 1 Becomes China's First Commercial Quantum Computer" . The Quantum Insider . 2 November 2025 . Retrieved 21 May 2026 . 
 ↑ "IBM Unveils World's First Integrated Quantum Computing System for Commercial Use" (Press release). IBM. 8 January 2019 . Retrieved 21 May 2026 . 
 ↑ Zhong, Han-Sen (2020). "Quantum computational advantage using photons". Science . 370 (6523): 1460– 1463. arXiv : 2012.01625 . Bibcode : 2020Sci...370.1460Z . doi : 10.1126/science.abe8770 . PMID   33273064 . 
 ↑ "QpiAI Launches 25-Qubit Superconducting Quantum System in India" . HPCwire . 16 April 2025 . Retrieved 21 May 2026 . 
 Sources [ edit ] 
 Aaronson, Scott (2013). Quantum Computing Since Democritus . Cambridge University Press. doi : 10.1017/CBO9780511979309 . ISBN   978-0-521-19956-8 . OCLC   829706638 . 
 Grumbling, Emily; Horowitz, Mark, eds. (2019). Quantum Computing: Progress and Prospects . Washington, DC: The National Academies Press. doi : 10.17226/25196 . ISBN   978-0-309-47970-7 . OCLC   1091904777 . S2CID   125635007 . 
 Mermin, N. David (2007). Quantum Computer Science: An Introduction . doi : 10.1017/CBO9780511813870 . ISBN   978-0-511-34258-5 . OCLC   422727925 . 
 Nielsen, Michael ; Chuang, Isaac (2010). Quantum Computation and Quantum Information (10th anniversary   ed.). doi : 10.1017/CBO9780511976667 . ISBN   978-0-511-99277-3 . OCLC   700706156 . S2CID   59717455 . 
 Further reading [ edit ] 
 3.0.CO;2-E"},"arxiv":{"wt":"quant-ph/0002077"},"bibcode":{"wt":"2000ForPh..48..771D"},"s2cid":{"wt":"15439711"}},"i":19}},"\n*",{"template":{"target":{"wt":"cite journal ","href":"./Template:Cite_journal"},"params":{"last":{"wt":"DiVincenzo"},"first":{"wt":"David P."},"title":{"wt":"Quantum Computation"},"journal":{"wt":"Science"},"year":{"wt":"1995"},"volume":{"wt":"270"},"issue":{"wt":"5234"},"pages":{"wt":"255–261"},"doi":{"wt":"10.1126/science.270.5234.255"},"bibcode":{"wt":"1995Sci...270..255D"},"s2cid":{"wt":"220110562"}},"i":20}}," Table 1 lists switching and dephasing times for various systems.\n*",{"template":{"target":{"wt":"cite journal ","href":"./Template:Cite_journal"},"params":{"last":{"wt":"Jeutner"},"first":{"wt":"Valentin"},"title":{"wt":"The Quantum Imperative: Addressing the Legal Dimension of Quantum Computers"},"journal":{"wt":"Morals & Machines"},"volume":{"wt":"1"},"pages":{"wt":"52–59"},"year":{"wt":"2021"},"doi":{"wt":"10.5771/2747-5174-2021-1-52"},"issue":{"wt":"1"},"s2cid":{"wt":"236664155"},"url":{"wt":"https://lup.lub.lu.se/record/e034e7b7-d17c-4863-9cee-7e654f97225b"},"doi-access":{"wt":"free"}},"i":21}},"\n*",{"template":{"target":{"wt":"Cite journal ","href":"./Template:Cite_journal"},"params":{"last1":{"wt":"Krantz"},"first1":{"wt":"P."},"last2":{"wt":"Kjaergaard"},"first2":{"wt":"M."},"last3":{"wt":"Yan"},"first3":{"wt":"F."},"last4":{"wt":"Orlando"},"first4":{"wt":"T. P."},"last5":{"wt":"Gustavsson"},"first5":{"wt":"S."},"last6":{"wt":"Oliver"},"first6":{"wt":"W. D."},"date":{"wt":"2019-06-17"},"title":{"wt":"A Quantum Engineer's Guide to Superconducting Qubits"},"journal":{"wt":"[[Applied Physics Reviews]]"},"language":{"wt":"en"},"volume":{"wt":"6"},"issue":{"wt":"2"},"page":{"wt":"021318"},"doi":{"wt":"10.1063/1.5089550"},"arxiv":{"wt":"1904.06560"},"bibcode":{"wt":"2019ApPRv...6b1318K"},"s2cid":{"wt":"119104251"},"issn":{"wt":"1931-9401"}},"i":22}},"\n*",{"template":{"target":{"wt":"cite web ","href":"./Template:Cite_web"},"params":{"last":{"wt":"Mitchell"},"first":{"wt":"Ian"},"year":{"wt":"1998"},"title":{"wt":"Computing Power into the 21st Century: Moore's Law and Beyond"},"url":{"wt":"http://citeseer.ist.psu.edu/mitchell98computing.html"}},"i":23}},"\n*",{"template":{"target":{"wt":"cite web ","href":"./Template:Cite_web"},"params":{"last":{"wt":"Simon"},"first":{"wt":"Daniel R."},"year":{"wt":"1994"},"title":{"wt":"On the Power of Quantum Computation"},"publisher":{"wt":"Institute of Electrical and Electronics Engineers Computer Society Press"},"url":{"wt":"http://citeseer.ist.psu.edu/simon94power.html"}},"i":24}},"\n",{"template":{"target":{"wt":"Refend","href":"./Template:Refend"},"params":{},"i":25}}]}'> 
 Textbooks [ edit ] 
 Benenti, Giuliano; Casati, Giulio; Rossini, Davide; Strini, Giuliano (2019). Principles of Quantum Computation and Information: A Comprehensive Textbook (2nd   ed.). doi : 10.1142/10909 . ISBN   978-981-3237-23-0 . OCLC   1084428655 . S2CID   62280636 . 
 Bernhardt, Chris (2019). Quantum Computing for Everyone . MIT Press. ISBN   978-0-262-35091-4 . OCLC   1082867954 . 
 Exman, Iaakov; Pérez-Castillo, Ricardo; Piattini, Mario; Felderer, Michael, eds. (2024). Quantum Software: Aspects of Theory and System Design . Springer Nature . doi : 10.1007/978-3-031-64136-7 . ISBN   978-3-031-64136-7 . 
 Hidary, Jack D. (2021). Quantum Computing: An Applied Approach (2nd   ed.). doi : 10.1007/978-3-030-83274-2 . ISBN   978-3-03-083274-2 . OCLC   1272953643 . S2CID   238223274 . 
 Hiroshi, Imai; Masahito, Hayashi , eds. (2006). Quantum Computation and Information: From Theory to Experiment . Topics in Applied Physics. Vol.   102. doi : 10.1007/3-540-33133-6 . ISBN   978-3-540-33133-9 . 
 Hughes, Ciaran; Isaacson, Joshua; Perry, Anastasia; Sun, Ranbel F.; Turner, Jessica (2021). Quantum Computing for the Quantum Curious . doi : 10.1007/978-3-030-61601-4 . ISBN   978-3-03-061601-4 . OCLC   1244536372 . S2CID   242566636 . 
 Jaeger, Gregg (2007). Quantum Information: An Overview . doi : 10.1007/978-0-387-36944-0 . ISBN   978-0-387-36944-0 . OCLC   186509710 . 
 Johnston, Eric R.; Harrigan, Nic; Gimeno-Segovia, Mercedes (2019). Programming Quantum Computers: Essential Algorithms and Code Samples . O'Reilly Media, Incorporated. ISBN   978-1-4920-3968-6 . OCLC   1111634190 . 
 Kaye, Phillip; Laflamme, Raymond ; Mosca, Michele (2007). An Introduction to Quantum Computing . OUP Oxford. ISBN   978-0-19-857000-4 . OCLC   85896383 . 
 Kitaev, Alexei Yu. ; Shen, Alexander H.; Vyalyi, Mikhail N. (2002). Classical and Quantum Computation . American Mathematical Soc. ISBN   978-0-8218-3229-5 . OCLC   907358694 . 
 Kurgalin, Sergei; Borzunov, Sergei (2021). Concise Guide to Quantum Computing: Algorithms, Exercises, and Implementations . Springer. doi : 10.1007/978-3-030-65052-0 . ISBN   978-3-030-65052-0 . 
 Stolze, Joachim; Suter, Dieter (2004). Quantum Computing: A Short Course from Theory to Experiment . doi : 10.1002/9783527617760 . ISBN   978-3-527-61776-0 . OCLC   212140089 . 
 Susskind, Leonard ; Friedman, Art (2014). Quantum Mechanics: The Theoretical Minimum . New York : Basic Books . ISBN   978-0-465-08061-8 . 
 Wichert, Andreas (2020). Principles of Quantum Artificial Intelligence: Quantum Problem Solving and Machine Learning (2nd   ed.). doi : 10.1142/11938 . ISBN   978-981-12-2431-7 . OCLC   1178715016 . S2CID   225498497 . 
 Wong, Thomas (2022). Introduction to Classical and Quantum Computing . Rooted Grove. ISBN   979-8-9855931-0-5 . OCLC   1308951401 . 
 Zeng, Bei; Chen, Xie; Zhou, Duan-Lu; Wen, Xiao-Gang (2019). Quantum Information Meets Quantum Matter . arXiv : 1508.02595 . doi : 10.1007/978-1-4939-9084-9 . ISBN   978-1-4939-9084-9 . OCLC   1091358969 . S2CID   118528258 . 
 Academic papers [ edit ] 
 Abbot, Derek ; Doering, Charles R. ; Caves, Carlton M. ; Lidar, Daniel M. ; Brandt, Howard E. ; et   al. (2003). "Dreams versus Reality: Plenary Debate Session on Quantum Computing". Quantum Information Processing . 2 (6): 449– 472. arXiv : quant-ph/0310130 . Bibcode : 2003QuIP....2..449A . doi : 10.1023/B:QINP.0000042203.24782.9a . hdl : 2027.42/45526 . S2CID   34885835 . 
 Berthiaume, Andre (1 December 1998). "Quantum Computation". Solution Manual for Quantum Mechanics . pp.   233– 234. doi : 10.1142/9789814541893_0016 . ISBN   978-981-4541-88-6 . S2CID   128255429 – via Semantic Scholar. 
 DiVincenzo, David P. (2000). "The Physical Implementation of Quantum Computation". Fortschritte der Physik . 48 ( 9– 11): 771– 783. arXiv : quant-ph/0002077 . Bibcode : 2000ForPh..48..771D . doi : 10.1002/1521-3978(200009)48:9/11 < 771::AID-PROP771 > 3.0.CO ; 2-E . S2CID   15439711 . 
 DiVincenzo, David P. (1995). "Quantum Computation". Science . 270 (5234): 255– 261. Bibcode : 1995Sci...270..255D . doi : 10.1126/science.270.5234.255 . S2CID   220110562 . Table 1 lists switching and dephasing times for various systems. 
 Jeutner, Valentin (2021). "The Quantum Imperative: Addressing the Legal Dimension of Quantum Computers" . Morals & Machines . 1 (1): 52– 59. doi : 10.5771/2747-5174-2021-1-52 . S2CID   236664155 . 
 Krantz, P.; Kjaergaard, M.; Yan, F.; Orlando, T. P.; Gustavsson, S.; Oliver, W. D. (17 June 2019). "A Quantum Engineer's Guide to Superconducting Qubits". Applied Physics Reviews . 6 (2): 021318. arXiv : 1904.06560 . Bibcode : 2019ApPRv...6b1318K . doi : 10.1063/1.5089550 . ISSN   1931-9401 . S2CID   119104251 . 
 Mitchell, Ian (1998). "Computing Power into the 21st Century: Moore's Law and Beyond" . {{ cite web }} : CS1 maint: miscellaneous url ( link ) 
 Simon, Daniel R. (1994). "On the Power of Quantum Computation" . Institute of Electrical and Electronics Engineers Computer Society Press. {{ cite web }} : CS1 maint: miscellaneous url ( link ) 
 External links [ edit ] 
 Media related to Quantum computer at Wikimedia Commons 
 Learning materials related to Quantum computing at Wikiversity 
 Stanford Encyclopedia of Philosophy : " Quantum Computing " by Amit Hagar and Michael E. Cuffaro 
 "Quantum computation, theory of" , Encyclopedia of Mathematics , EMS Press, 2001 [1994] 
 Introduction to Quantum Computing for Business by Koen Groenland 
 Schneider, J., & Smalley, I. (2024, August 5). What Is Quantum Computing? | IBM . What Is Quantum Computing? | IBM 
 Lectures 
 Quantum computing for the determined – 22 video lectures by Michael Nielsen 
 Video Lectures Archived 10 February 2010 at the Wayback Machine by David Deutsch 
 Lomonaco, Sam. Four Lectures on Quantum Computing given at Oxford University in July 2006 
 v t e Processor technologies Models 
 Abstract machine 
 Stored-program computer 
 Finite-state machine 
 with datapath 
 Hierarchical 
 Deterministic finite automaton 
 Queue automaton 
 Cellular automaton 
 Quantum cellular automaton 
 Turing machine 
 Alternating Turing machine 
 Universal 
 Post–Turing 
 Quantum 
 Nondeterministic Turing machine 
 Probabilistic Turing machine 
 Hypercomputation 
 Zeno machine 
 Belt machine 
 Stack machine 
 Register machines 
 Counter 
 Pointer 
 Random-access 
 Random-access stored program 
 Architecture 
 Microarchitecture 
 Von Neumann 
 Harvard 
 modified 
 Dataflow 
 Transport-triggered 
 Cellular 
 Endianness 
 Memory access 
 NUMA 
 HUMA 
 Load–store 
 Register/memory 
 Cache hierarchy 
 Memory hierarchy 
 Virtual memory 
 Secondary storage 
 Heterogeneous 
 Fabric 
 Multiprocessing 
 Cognitive 
 Neuromorphic 
 Instruction set architectures Types 
 Orthogonal instruction set 
 CISC 
 RISC 
 Application-specific 
 EDGE 
 TRIPS 
 VLIW 
 EPIC 
 MISC 
 OISC 
 NISC 
 ZISC 
 VISC architecture 
 Quantum computing 
 Comparison 
 Addressing modes 
 Instruction sets 
 Motorola 68000 series 
 VAX 
 PDP-11 
 x86 
 ARM 
 Stanford MIPS 
 MIPS 
 MIPS-X 
 Power
 POWER 
 PowerPC 
 Power ISA 
 Clipper architecture 
 SPARC 
 SuperH 
 DEC Alpha 
 ETRAX CRIS 
 M32R 
 Unicore 
 Itanium 
 OpenRISC 
 RISC-V 
 MicroBlaze 
 LMC 
 System/3x0
 S/360 
 S/370 
 S/390 
 z/Architecture 
 Tilera ISA 
 VISC architecture 
 Epiphany architecture 
 Others 
 Execution Instruction pipelining 
 Pipeline stall 
 Operand forwarding 
 Classic RISC pipeline 
 Hazards 
 Data dependency 
 Structural 
 Control 
 False sharing 
 Out-of-order 
 Scoreboarding 
 Tomasulo's algorithm 
 Reservation station 
 Re-order buffer 
 Register renaming 
 Wide-issue 
 Speculative 
 Branch prediction 
 Memory dependence prediction 
 Parallelism Level 
 Bit 
 Bit-serial 
 Word 
 Instruction 
 Pipelining 
 Scalar 
 Superscalar 
 Task 
 Thread 
 Process 
 Data 
 Vector 
 Memory 
 Distributed 
 Multithreading 
 Temporal 
 Simultaneous 
 Hyperthreading 
 Simultaneous and heterogenous 
 Speculative 
 Preemptive 
 Cooperative 
 Flynn's taxonomy 
 SISD 
 SIMD 
 Array processing (SIMT) 
 Pipelined processing 
 Associative processing 
 SWAR 
 MISD 
 MIMD 
 SPMD 
 Processor performance 
 Transistor count 
 Instructions per cycle (IPC)
 Cycles per instruction (CPI) 
 Instructions per second (IPS) 
 Floating-point operations per second (FLOPS) 
 Transactions per second (TPS) 
 Synaptic updates per second (SUPS) 
 Performance per watt (PPW) 
 Cache performance metrics 
 Computer performance by orders of magnitude 
 Types 
 Central processing unit (CPU) 
 Graphics processing unit (GPU)
 GPGPU 
 Vector 
 Barrel 
 Stream 
 Tile processor 
 Coprocessor 
 PAL 
 ASIC 
 FPGA 
 FPOA 
 CPLD 
 Multi-chip module (MCM) 
 System in a package (SiP) 
 Package on a package (PoP) 
 By application 
 Embedded system 
 Microprocessor 
 Microcontroller 
 Mobile 
 Ultra-low-voltage 
 ASIP 
 Soft microprocessor 
 Systems on chip 
 System on a chip (SoC) 
 Multiprocessor (MPSoC) 
 Cypress PSoC 
 Network on a chip (NoC) 
 Hardware accelerators 
 Coprocessor 
 AI accelerator 
 Graphics processing unit (GPU) 
 Image processor 
 Vision processing unit (VPU) 
 Physics processing unit (PPU) 
 Digital signal processor (DSP) 
 Tensor Processing Unit (TPU) 
 Secure cryptoprocessor 
 Network processor 
 Baseband processor 
 Word size 
 1-bit 
 4-bit 
 8-bit 
 12-bit 
 15-bit 
 16-bit 
 24-bit 
 32-bit 
 48-bit 
 64-bit 
 128-bit 
 256-bit 
 512-bit 
 bit slicing 
 others 
 variable 
 Core count 
 Single-core 
 Multi-core 
 Manycore 
 Heterogeneous architecture 
 Components 
 Core 
 Cache 
 CPU cache 
 Scratchpad memory 
 Data cache 
 Instruction cache 
 replacement policies 
 coherence 
 Bus 
 Clock rate 
 Clock signal 
 FIFO 
 Functional units 
 Arithmetic logic unit (ALU) 
 Address generation unit (AGU) 
 Floating-point unit (FPU) 
 Memory management unit (MMU)
 Load–store unit 
 Translation lookaside buffer (TLB) 
 Branch predictor 
 Branch target predictor 
 Integrated memory controller (IMC)
 Memory management unit 
 Instruction decoder 
 Logic 
 Combinational 
 Sequential 
 Glue 
 Logic gate 
 Quantum 
 Array 
 Registers 
 Processor register 
 Status register 
 Stack register 
 Register file 
 Memory buffer 
 Memory address register 
 Program counter 
 Control unit 
 Hardwired control unit 
 Instruction unit 
 Data buffer 
 Write buffer 
 Microcode 
 ROM 
 Counter 
 Datapath 
 Multiplexer 
 Demultiplexer 
 Adder 
 Multiplier 
 CPU 
 Binary decoder 
 Address decoder 
 Sum-addressed decoder 
 Barrel shifter 
 Circuitry 
 Integrated circuit 
 3D 
 Mixed-signal 
 Power management 
 Boolean 
 Digital 
 Analog 
 Quantum 
 Switch 
 Power management 
 PMU 
 APM 
 ACPI 
 Dynamic frequency scaling 
 Dynamic voltage scaling 
 Clock gating 
 Performance per watt (PPW) 
 Related 
 History of general-purpose CPUs 
 Microprocessor chronology 
 Processor design 
 Digital electronics 
 Hardware security module 
 Semiconductor device fabrication 
 Tick–tock model 
 Pin grid array 
 Chip carrier 
 v t e Quantum information science General 
 DiVincenzo's criteria 
 NISQ era 
 Quantum computing 
 timeline 
 Quantum information 
 Quantum programming 
 Quantum simulation 
 Qubit 
 physical vs. logical 
 Quantum processors 
 cloud-based 
 Theorems 
 Bell's 
 Eastin–Knill 
 Gleason's 
 Gottesman–Knill 
 Holevo's 
 No-broadcasting 
 No-cloning 
 No-communication 
 No-deleting 
 No-hiding 
 No-teleportation 
 PBR 
 Quantum speed limit 
 Threshold 
 Solovay–Kitaev 
 Schrödinger-HJW 
 Quantum communication 
 Classical capacity 
 entanglement-assisted 
 quantum capacity 
 Entanglement distillation 
 Entanglement swapping 
 Monogamy of entanglement 
 LOCC 
 Quantum channel 
 quantum network 
 State purification 
 Quantum teleportation 
 quantum energy teleportation 
 quantum gate teleportation 
 Superdense coding 
 Quantum cryptography 
 Decoy state 
 Hidden matching 
 Post-quantum cryptography 
 Quantum coin flipping 
 Quantum money 
 Quantum key distribution 
 BB84 
 SARG04 
 other protocols 
 Quantum secret sharing 
 Quantum algorithms 
 Algorithmic cooling 
 Amplitude amplification 
 Bernstein–Vazirani 
 BHT 
 Boson sampling 
 Deutsch–Jozsa 
 Grover's 
 HHL 
 Hidden subgroup 
 Magic state distillation 
 Quantum annealing 
 Quantum counting 
 Quantum Fourier transform 
 Quantum optimization 
 Quantum phase estimation 
 Shor's 
 Simon's 
 VQE 
 Quantum complexity theory 
 BQP 
 DQC1 
 EQP 
 QIP 
 QMA 
 PostBQP 
 Quantum processor benchmarks 
 Quantum supremacy 
 Quantum volume 
 QC scaling laws 
 Randomized benchmarking 
 XEB 
 Relaxation times 
 T 1 
 T 2 
 Quantum computing models 
 Adiabatic quantum computation 
 Continuous-variable quantum information 
 One-way quantum computer 
 cluster state 
 Quantum circuit 
 quantum logic gate 
 Quantum machine learning 
 quantum neural network 
 Quantum Turing machine 
 Topological quantum computer 
 Hamiltonian quantum computation 
 Quantum error correction 
 Codes
 5 qubit 
 CSS 
 GKP 
 quantum convolutional 
 stabilizer 
 Shor 
 Bacon–Shor 
 Steane 
 Toric 
 gnu 
 Entanglement-assisted 
 Physical implementations Quantum optics 
 Cavity QED 
 Circuit QED 
 Linear optical QC 
 KLM protocol 
 Ultracold atoms 
 Neutral atom QC 
 Trapped-ion QC 
 Spin -based 
 Kane QC 
 Spin qubit QC 
 NV center 
 NMR QC 
 Superconducting 
 Charge qubit 
 Flux qubit 
 Phase qubit 
 Transmon 
 Quantum programming 
 OpenQASM – Qiskit – IBM QX 
 Quil – Forest/Rigetti QCS 
 Cirq 
 Q# 
 libquantum 
 many others... 
 Quantum information science 
 Quantum mechanics topics 
 v t e Emerging technologies Fields Quantum 
 algorithms 
 amplifier 
 bus 
 cellular automata 
 channel 
 circuit 
 complexity theory 
 computing 
 cryptography 
 post-quantum 
 dynamics 
 electronics 
 error correction 
 finite automata 
 image processing 
 imaging 
 information 
 key distribution 
 logic 
 logic clock 
 logic gate 
 machine 
 machine learning 
 metamaterial 
 network 
 neural network 
 optics 
 programming 
 sensing 
 simulator 
 teleportation 
 Other 
 Acoustic levitation 
 Anti-gravity 
 Cloak of invisibility 
 Digital scent technology 
 Force field 
 Plasma window 
 Immersive virtual reality 
 Magnetic refrigeration 
 Phased-array optics 
 Thermoacoustic heat engine 
 List 
 v t e Quantum mechanics Background 
 Introduction 
 History 
 Timeline 
 Classical mechanics 
 Old quantum theory 
 Glossary 
 Fundamentals 
 Born rule 
 Bra–ket notation 
 Complementarity 
 Density matrix 
 Energy level 
 Ground state 
 Excited state 
 Degenerate levels 
 Zero-point energy 
 Entanglement 
 Hamiltonian 
 Interference 
 Decoherence 
 Measurement 
 Nonlocality 
 Quantum state 
 quantum jump 
 Superposition 
 Tunnelling 
 Scattering theory 
 Symmetry in quantum mechanics 
 Uncertainty 
 Wave function 
 Collapse 
 Wave–particle duality 
 Universal wave function 
 Formulations 
 Formulations 
 Heisenberg 
 Interaction 
 Matrix mechanics 
 Schrödinger 
 Path integral formulation 
 Phase space 
 Equations 
 Klein–Gordon 
 Dirac 
 Weyl 
 Majorana 
 Rarita–Schwinger 
 Pauli 
 Rydberg 
 Schrödinger 
 Interpretations 
 Bayesian 
 Consciousness causes collapse 
 Consistent histories 
 Copenhagen 
 de Broglie–Bohm 
 Ensemble 
 Hidden-variable 
 Local 
 Superdeterminism 
 Many-worlds 
 Objective collapse 
 Quantum logic 
 Relational 
 Transactional 
 Experiments 
 Bell test 
 Davisson–Germer 
 Delayed-choice quantum eraser 
 Double-slit 
 Franck–Hertz 
 Mach–Zehnder interferometer 
 Elitzur–Vaidman 
 Popper 
 Quantum eraser 
 Stern–Gerlach 
 Wheeler's delayed choice 
 Science 
 Quantum biology 
 Quantum chemistry 
 Quantum chaos 
 Quantum cosmology 
 Quantum differential calculus 
 Quantum dynamics 
 Quantum geometry 
 Quantum measurement problem 
 Quantum mind 
 Quantum stochastic calculus 
 Quantum spacetime 
 Technology 
 Quantum algorithms 
 Quantum amplifier 
 Quantum bus 
 Quantum cellular automata 
 Quantum finite automata 
 Quantum channel 
 Quantum circuit 
 Quantum complexity theory 
 Quantum computing 
 Timeline 
 Quantum cryptography 
 Quantum electronics 
 Quantum error correction 
 Quantum imaging 
 Quantum image processing 
 Quantum information 
 Quantum key distribution 
 Quantum logic 
 Quantum logic gates 
 Quantum machine 
 Quantum machine learning 
 Quantum metamaterial 
 Quantum metrology 
 Quantum network 
 Quantum neural network 
 Quantum optics 
 Quantum programming 
 Quantum sensing 
 Quantum simulator 
 Quantum teleportation 
 Extensions 
 Quantum fluctuation 
 Casimir effect 
 Quantum statistical mechanics 
 Quantum field theory 
 History 
 Quantum gravity 
 Relativistic quantum mechanics 
 Related 
 Schrödinger's cat 
 in popular culture 
 Wigner's friend 
 EPR paradox 
 Quantum mysticism 
 Category 
 Authority control databases National United States kvantové výpočty</span>"}]]}'> Czech Republic Israel Other Yale LUX 
 Retrieved from " https://en.wikipedia.org/w/index.php?title=Quantum_computing&oldid=1369455350 " 
 Categories : Quantum computing Models of computation Quantum cryptography Information theory Computational complexity theory Classes of computers Theoretical computer science Open problems Computer-related introductions in 1980 Supercomputers Hidden categories: Articles with short description Short description is different from Wikidata Use American English from February 2023 All Wikipedia articles written in American English Use dmy dates from February 2021 CS1 Russian-language sources (ru) Articles needing additional references from July 2026 All articles needing additional references CS1 maint: deprecated archival service CS1 maint: bot: original URL status unknown All articles with dead external links Articles with dead external links from November 2025 Articles containing potentially dated statements from 2023 All articles containing potentially dated statements All articles with unsourced statements Articles with unsourced statements from July 2026 CS1 maint: miscellaneous url Commons link is locally defined Webarchive template wayback links 
 This page was last edited on 15 August 2026, at 02:31  (UTC) . 
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
 Quantum computing 
 44 languages 
 Add topic