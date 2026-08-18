Optimized surface ion trap design for tight confinement and separation of ion chains 
 Report GitHub Issue 
 × 
 Title: 
 Content selection saved. Describe the issue below: 
 Description: 
 Submit without GitHub 
 Submit in GitHub 
 arXiv is now an independent nonprofit! 
 Learn more 
 × 
 Back to arXiv 
 Why HTML? 
 Report Issue 
 Back to Abstract 
 Download PDF 
 Аннотация 
 1 Introduction 
 2 Methods 
 3 Basic design 
 4 Asymmetric trap 
 5 Ion chain separation 
 6 Conclusion 
 Список литературы 
 License: CC BY 4.0
arXiv:2407.14195v1 [quant-ph] 19 Jul 2024 
 \lat \rtitle 
 Optimized surface ion trap…
 \sodtitle Optimized surface ion trap design for tight confinement and separation of ion chains
 \rauthor I. S. Gerasin, N. O. Zhadnov, K. S. Kudeyarov, K. Y. Khabarova, N. N. Kolachevsky,
I. A. Semerikov
 \sodauthor Gerasin, Zhadnov, Kudeyarov, Khabarova, Kolachevsky, Semerikov
 \PACS 74.50.+r, 74.80.Fp 
 Optimized surface ion trap design for tight confinement and separation of ion chains 
 I. S. Gerasin 1,2,3, 
 Thanks: e-mail: i.gerasin@rqc.ru
 N. O. Zhadnov 1 
 K. S. Kudeyarov 1 
 K. Y. Khabarova 1 
 N. N. Kolachevsky 1,2 
 I. A. Semerikov 1,2 
 Address: 1 P.N. Lebedev Physical Institute, Russian Academy
of Sciences, Moscow, Russia; 
 2 Russian Quantum Center, Moscow, Russia 
 3 Moscow Institute of Physics and Technology, Dolgoprudny, Russia 
 Аннотация 
 Qubit systems based on trapped ultracold ions win one of the leading positions in the quantum computing field, demonstrating quantum algorithms with the highest complexity to date. Surface Paul traps for ion confinement open the opportunity to scale quantum processors to hundreds of qubits and enable high-connectivity manipulations on ions.
To fabricate such a system with certain characteristics, the special design of a surface electrode structure is required. The depth of the trapping potential, the stability parameter, the secular frequency and the distance between an ion and the trap surface should be optimized for better performance. Here we present the optimized design of a relatively simple surface trap that allows several important high-fidelity primitives: tight ion confinement, laser cooling, and wide optical access. The suggested trap design also allows to perform an important basic operation, namely, splitting an ion chain into two parts. 
 1 Introduction 
 The field of quantum computing is rapidly advancing. Using qubits as information carriers allows the implementation of new algorithms, which can overcome classical computing [ 1 , 2 ] . One of the possible ways of qubits realization is encoding the internal energy levels in atomic ions [ 3 ] which are confined by RF and DC electric fields in Paul traps [ 4 ] and entangling ions via common vibrational modes in the trap. Ions are a promising platform for quantum computing due to their long lifetime in the trap, long coherence times of the qubit levels, straightforward initialization and readout, and strong Coloumb interaction between particles.
Additionally, ultracold trapped ions are potential candidates for implementing quantum memristors [ 5 ] , which are promising elements for neuromorphic (biomimetic) computing systems. This is due to their numerous ion levels with varying lifetimes and transitions of different "oscillator strengths"  as well as the ability of using full connectivity via vibrational modes [ 6 , 7 ] . 
 One of the most pronounced challenges in advancing quantum computers lies in scaling them up, namely, increasing the number of individually controlled qubits while upholding quantum gate fidelities, low decoherence, and high connectivity [ 8 ] . 3D Paul traps consisting of four massive RF electrodes allow to confine more than 100 ions in a linear chain useful for computation [ 9 ] . However, addressing individual ions in such lengthy chains poses a significant challenge due to the small interionic distances. Besides, maintaining the high fidelity of two-qubit gates begins to require an extremely high level of quantum state control due to the complication of the motional-mode spectrum of multi-ion chains [ 10 ] .
To overcome these issues, the ion chain can be divided into smaller modules (sub-chains) by an external electric field. One can perform operations separately on such sub-chains and subsequently transfer quantum information between them by physically joining them up.
In 2002 the quantum charge-coupled device (QCCD) architecture was proposed [ 11 ] . In this approach, the ion trap is divided into several zones [ 12 ] devoted to specific operation types, such as loading, initialization, quantum gates, storage, and readout. The advantage of such an architecture lies in its greater efficiency in utilizing experimental resources, such as lasers, by enabling the movement of ions into the interaction zone. This approach eliminates the need to create dedicated laser and optical systems for each ion. 
 To implement such an architecture, the concept of a linear Paul trap was converted to the form of a 2D microfabricated chip comprising a pattern of metal electrodes on the surface of a dielectric (quartz, silica, sapphire) or silicon substrate [ 13 , 14 ] . The manufacturing is based on well-established photolithographic techniques but also demands the use of the most suitable conducting and insulating materials [ 15 ] , specific methods to increase the breakdown voltage [ 16 , 17 ] , formation and pattering of thick dielectric layers, as well as minimizing the surface of dielectric observed by trapped ions due to patch charges.
The most impressive quantum computing performance to date has been shown by IonQ [ 18 ] and Quantinuum [ 19 ] companies, which demonstrated successful manipulation with 30 and 32 ion qubits in surface traps, respectively. Notably, Quantinuum achieved a remarkable quantum volume of 2 16 2^{16} . 
 A basic design of a microfabricated surface trap is presented in Fig. 1 . In this trap the radiofrequency (RF) voltage with an amplitude V r ​ f V_{rf} and a frequency f r ​ f f_{rf} is applied to a pair of electrodes. Between the RF electrodes, there is a grounded central electrode, forming the pseudopotential in the x-y plane that provides radial confinement of ions. Segmented outer electrodes are used to provide axial confinement (z-axis), relocate ions across the z-axis, and compensate micromotion [ 20 ] . 
 Surface traps make it relatively straightforward to perform the operations of separating and joining ion chains (Fig. 2 ). These operations are crucial for the realization of a flexible modular architecture, improving the fidelity of two-qubit gates, implementing two-qubit gates (such as physical SWAP) between ions in different chains, realizing error correction codes, and creating on-chip distributed quantum networks [ 8 ] . To execute these operations, outer or center segmented DC electrodes can be used [ 21 ] . 
 Рис. 1: Fig. 1. Basic surface ion trap design: left - view along the trap axis, right - top view. 
 There is a number of studies focused on finding the optimal design for surface traps using both analytical models [ 22 ] and numerical simulations [ 21 , 23 , 24 ] . In general, surface traps can consist of a large and complex system of electrodes, which are more easily described through computer simulations than by analytical methods. The main challenge in optimizing trap geometry lies in the numerous interconnected specifications that must be simultaneously optimized: trap depth, ion-to-trap distance, secular frequency, and stability parameter. 
 In this work, we investigate the influence of electrode size and geometry on the key trap parameters through simulations to identify a design that ensures robust confinement, efficient laser cooling and addressing, and the capability for ion chain separation.
Section 2 of this article describes the trap parameters and the calculation method. In Section 3, we optimize the electrode sizes for a basic surface trap. Sections 4 and 5 focus on designing a trap with asymmetric electrodes and a surface structure tailored for ion chain separation.
We hope the results of this study will serve as a valuable guide for designing surface ion traps. 
 Рис. 2: Fig. 2. Illustration of dividing a chain of 6 ions into two sub-chains. a - microchip with a surface ion trap, b - chain of 6 ions, c - two sub-chains after separation. The bow tie shape is standard for QCCD microchips, allowing wider optical access to the central ion trapping region. 
 2 Methods 
 Calculations were made for 171 Yb + as a qubit. There are several transitions in this ion to encode quantum information with long coherence times which are regularly used in quantum computing [ 25 , 26 ] , including our previous works [ 27 , 28 , 29 , 30 ] .
Also it is worth note, that 171 Yb + ion is extremely promising for the creation of quantum memristors [ 6 , 7 ] . 
 To achieve better performance, the following trap parameters should be controlled: 
 • 
 radial secular frequencies f s ​ e ​ c f_{sec} characterizing the potential well: 
 2 ​ π ​ f s ​ e ​ c = ϵ ​ Q ​ V r ​ f 2 ​ m ​ h 2 ​ 2 ​ π ​ f r ​ f 2\pi f_{sec}=\frac{\epsilon QV_{rf}}{\sqrt{2}mh^{2}2\pi f_{rf}} 
 (1) 
 where Q , m Q,m - the charge and the mass of the ion, h h - the distance to the nearest electrode (in our case the distance to the surface), ϵ \epsilon - the efficiency parameter (typically 0.2 - 0.3 for surface traps) [ 31 ] . Usually, f s ​ e ​ c f_{sec} is chosen to be about 2 − 4 2-4\, MHz to provide the Lamb Dicke regime [ 29 , 32 , 33 ] . 
 • 
 trap potential depth. It should be deep enough to capture particles produced by photoionization of the neutral atomic beam from the hot gun. The temperature of the atomic gun is about 600 600 K which corresponds to particle energy of ≈ 0.05 \approx 0.05 eV. 
 • 
 Mathieu stability parameter q q : 
 q = 2 ​ 2 ​ f s ​ e ​ c f r ​ f = 2 ​ ϵ ​ Q ​ V r ​ f m ​ h 2 ​ ( 2 ​ π ​ f r ​ f ) 2 , q=2\sqrt{2}\frac{f_{sec}}{f_{rf}}=\frac{2\epsilon QV_{rf}}{mh^{2}(2\pi f_{rf})^{2}}, 
 (2) 
 The stability parameter should be small enough ( q 2 ≪ 1 q^{2}\ll 1 ) to maintain the harmonicity of ion oscillations. However, trap depth is proportional to this parameter [ 34 ] . 
 • 
 the distance from the ion to the trap surface h h that defines the optical access required for laser cooling and quantum operations and impacts the ion heating rate induced by the surface ( ∝ 1 / h 4 \propto 1/h^{4} ) [ 35 ] . The optical access is determined by the numerical aperture (NA) for a beam propagating parallel to the trap plane and strongly focused on the ion. The NA is limited by the width of the isthmus of the trap (Fig. 2 ), which is typically around 1 mm. With an ion height above the trap surface ranging from 70 70 to 100 100 μ \mu m, the NA will be between 0.14 and 0.19 for a beam perpendicular to the isthmus. Such a setup will accommodate beam waists of < 2 <2 μ \mu m at the ion, which is sufficient for individual addressing. 
 The trap was simulated using a Python package Electrode [ 36 ] , which allows one to evaluate the field distribution and pseudopotential parameters depending on the trap geometry. By searching through different configurations of electrodes, one can optimise the electrode structure for the desired confining potential.
The RF voltage amplitude is usually limited to several hundred volts due to electrical breakdown [ 16 , 17 ] . We take a conservative estimation and set the amplitude V r ​ f = 100 V_{rf}=100\, V.
We fix the q q parameter around 0.3, which is close to the optimal value, ensuring sufficient depth of the trapping potential. Since we use numerical estimations of the parameters, we can not always find a solution for exact value of q q , so we search for it in the interval q = 0.300 ± 0.008 q=0.300\pm 0.008 . If we want to consider higher voltage amplitudes while conserving q q , we should increase the RF driving frequency f r ​ f f_{rf} (Eq. 2 ). These adjustments also result in the increase of f s ​ e ​ c f_{sec} , which is advantageous for the Lamb Dicke regime and the time of quantum gates. We assume that the frequency f r ​ f ∈ [ 20 , 24 ] f_{rf}\in[20,24]\, MHz which corresponds to f s ​ e ​ c ≈ 2 f_{sec}\approx 2\, MHz. By pre-setting the parameters ( V r ​ f , q , f s ​ e ​ c V_{rf},q,f_{sec} ), we manipulate the geometry of the symmetric surface trap to optimize the distance h h and the depth of the trapping potential. 
 3 Basic design 
 We consider the planar trap configuration as depicted in Fig. 1 . The width of the central electrode is defined as w c w_{c} , while the width of the RF electrodes is defined as w r w_{r} . We variate them in the following ranges: w c ∈ [ 30,300 ] w_{c}\in[30,300]\, μ \mu m, w r ∈ [ 30,300 ] w_{r}\in[30,300]\, μ \mu m. The outer electrodes are considered to be grounded and 1 mm wide. The gap between neighboring electrodes is set to 6 μ \mu m due to constraints associated with the fabrication process and the limitations related to the RF breakdown. 
 The first step is to find the configuration of electrodes which optimizes the distance from the ion to the trap surface h h . This parameter is solely determined by the trap geometry and is independent of voltage settings. In contrast, the secular frequencies, the stability parameter, and the potential depth are determined by both the amplitude and the frequency of the RF field. 
 The dependence between the electrode’s widths and the distance h h is presented in Fig. 3 . Notable, that increasing of w c w_{c} and w r w_{r} results in the increase of the distance h h . It results from the linear scaling between the trap geometry and trapping potential. The points with fixed h h correspond to different potential depths, secular, and driving voltage frequencies. So, for the specified h h , there exist several degrees of freedom, enabling adjustments in the widths of the electrodes. 
 Рис. 3: Fig. 3. Graphs show the dependence of the distance from the ion to the trap surface h h on the sizes of electrodes w r w_{r} and w c w_{c} .
Parameters q q and f s ​ e ​ c f_{sec} vary along the curves. 
 Fig. 4 represents the dependencies between w c w_{c} and w r w_{r} for the case when the stability parameter q q and the driving frequency f r ​ f f_{rf} (and therefore, the secular frequency f s ​ e ​ c f_{sec} ) are fixed. 
 Рис. 4: Fig. 4. Dependence of w r w_{r} as a function of w c w_{c} for fixed q q = 0.3, V r ​ f V_{rf} = 100 V and three different values of f r ​ f f_{rf} . 
 The plots presented in Fig. 3 and Fig. 4 allow us to select a trap geometry satisfying the required parameters.
Fixing the distance h h , the driving frequency f r ​ f f_{rf} , and the stability parameter q q defines a pair of curves. The intersection of these curves corresponds to the target geometry. Notably, there may be a combination of parameters for which the desired distance h h is not achievable because the curves do not intersect. Decreasing h h , we obtain one or two solutions defining trap geometry. 
 Next, we consider the potential depth. Fig. 5 shows the dependence between it and the parameter h h . Each group of points of the same color corresponds to a certain value of f r ​ f f_{rf} . The qualitative behavior can be described in the same way as in the previous paragraph. If h h is too large, no solutions exist. By lowering h h , one value of the potential depth becomes possible. By further reducing h h , we observe two achievable solutions, each corresponding to one of two intersections between lines with certain values of h h and f r ​ f f_{rf} given in Fig. 3 and 4 . 
 Рис. 5: Fig. 5. Dependence of the trapping potential depth as a function of the distance h h , V r ​ f V_{rf} = 100 V. The scatter of points comes from a small variation of q = 0.300 ± 0.008 q=0.300\pm 0.008 . 
 By selecting the point with the maximum h h for any given f r ​ f f_{rf} , we achieve a potential depth of more than 0.1 eV, sufficient for ion trapping. As shown in Fig. 5 , increasing the secular frequency reduces h h , requiring a balanced solution. We determine the optimal values to be f r ​ f = 22 f_{rf}=22\, MHz and h ≈ 80 h\approx 80\, μ \mu m. This region corresponds to w c ∈ [ 40 , 60 ] w_{c}\in[40,60]\, μ \mu m and w r ∈ [ 140,200 ] w_{r}\in[140,200]\, μ \mu m. 
 4 Asymmetric trap 
 The trap presented in Fig. 1 is symmetric, which means that both RF electrodes have the same width. In this case, two modes of the secular motion coincide with the x x and y y axes. This means that a cooling laser beam parallel to the trap surface will not effectively cool the motion perpendicular to the trap surface. To address this issue, an asymmetric trap with differing RF electrode widths can be employed. In such a configuration, both mode axes have a projection onto the laser beam, enabling effective cooling along both axes. 
 We denote the angle between the normal to the surface and the secular mode direction (closest to the vertical) as α \alpha and consider the design of an asymmetric trap. Assume that the RF electrodes have different widths: w r u w_{r}^{u} and w r d w_{r}^{d} . The central electrode of width w c w_{c} and outer DC electrodes remain grounded. To tilt the direction of secular modes, we vary w c w_{c} , w r u w_{r}^{u} in the boundaries defined by the previous analysis (this allows us to reduce the calculation grid) and w r d w_{r}^{d} in the range [ 140,500 ] [140,500]\, μ \mu m. The angle α \alpha is considered to be in the range from 10 ∘ 10^{\circ} to 20 ∘ 20^{\circ} . On the one hand, α \alpha should be big enough to ensure effective cooling on both axes; on the other hand, better symmetry is usually more optimal in terms of trap parameters. The driving frequency and the field amplitude are fixed at f r ​ f = 22 f_{rf}=22\, MHz and V r ​ f = 100 V_{rf}=100\, V. The program computed the Hessian of the pseudopotential at its minimum. By diagonalizing the resulting matrix, we determined the directions of the vibrational axes. The resulting solutions were post-selected to satisfy the criteria h ≈ h\approx 80 μ ​ m \mu m , q ≈ 0.3 q\approx 0.3 , α ∈ [ 10 , 20 ] ∘ \alpha\in[10,20]^{\circ} . Finally, the optimal solution was chosen that maximizes the trap depth. Its parameters are summarized in Table 0 . 
 w c w_{c} 
 w r d w_{r}^{d} 
 w r u w_{r}^{u} 
 V r ​ f V_{rf} 
 f r ​ f f_{rf} 
 40 μ ​ m \mu m 
 160 μ ​ m \mu m 
 400 μ ​ m \mu m 
 100 V 
 22 MHz 
 q q 
 h h 
 x 0 x_{0} 
 depth 
 α \alpha 
 0.3 
 80 μ ​ m \mu m 
 -11 μ ​ m \mu m 
 110 meV 
 14 ∘ 
 Таблица 0: Table 1. Optimal parameters of the asymmetric trap for the considered calculation grid. x 0 x_{0} is the displacement of the ion across the x x -axis caused by the trap asymmetry. 
 The corresponding pseudopotential distribution and principal axes of secular motion are presented in Fig. 6 . The equilibrium position is above the x − z x-z plane at the height 80 μ ​ m \mu m and is shifted by x 0 = 11 ​ μ ​ m x_{0}=11\>\mu m along the x x -axis towards the thin RF electrode. The principal axes of secular motion are rotated by the angle of 14 ∘ towards this electrode. 
 Рис. 6: Fig. 6. Pseudopotential distribution above the trap surface in x − y x-y plane and axes of secular motion (white). The parameters of the trap are presented in Table 0 . 
 5 Ion chain separation 
 The next step is to design the trapping potential along the z z -axis. We simulate the lengths and positions of the outer electrodes that modify the axial DC potential, thereby defining the movement of particles across the z-axis. The separation of the ion chain into two parts, the basic operation of modern QCCD architecture, should be optimized to provide robust and reproducible operations. A chain of ions in an axial trapping potential can be separated by creating a potential barrier between neighboring ions (Fig. 2 ). The challenge in precisely dividing an ion chain lies in the fact that inter-ionic distances are typically less than 10 μ ​ m \mu m , while the distance to the electrodes is at least an order of magnitude greater. Under these conditions, it is impossible to create an electric potential peak narrow enough to resolve the distance between ions. Therefore, the most effective strategy for localizing chain separation is to minimize the width of the potential barrier. 
 Fig. 7 gives the suggested trap design. The trap consists of two sections, which comprise three pairs of electrodes: the central pair of r 2 r_{2} width (depicted as gray in Fig. 7 ), and two side pairs (green) with the width of r 1 r_{1} . The trap center has an electrode (colored yellow) with width r 0 r_{0} to create a separating "wedge" potential barrier. We will consider the initial situation of a single ion chain confined in the very center of the trap by applying a positive potential to the pairs of grey electrodes and a negative potential to the pairs of central green and yellow electrodes. At the end of the splitting procedure, each of the two sections confines its separate ion chain. Here we will consider the optimal geometry of the electrodes involved in the process of chain splitting. The dynamics of ions during the separation procedure and the recapture process of sub-chains into the corresponding trap zones are beyond the scope of this work. 
 Рис. 7: Fig. 7. Design of outer DC electrodes for ion chain separation. Here, r 0 r_{0} is the length of the central separation electrode, r 1 r_{1} is the length of the locking electrodes, and r 2 r_{2} is the length of the middle electrode within each section. 
 At the start of the splitting procedure 10 10\, V is applied to the central (grey) electrodes and − 10 -10\, V to the side (green) electrodes near the separation electrode; all outer side electrodes are grounded. Then one increases the voltage u u on the separation electrode (yellow) from -5 V to 10 V to raise the barrier and create a double-well structure. The evolution of the resulting axial potential is shown in Fig. 8 .
Voltages in the range of 10 10 V are easily achievable in a laboratory without getting much voltage noise. 
 Рис. 8: Fig. 8. Trap potential along the axial direction. The configuration of RF electrodes is presented in Table 0 , and the configuration of DC electrodes is presented in Table 1 . Colored rectangles at the bottom represent the lengths and z-axis positions of outer electrodes; colors correspond to Fig. 7 . 
 To ensure effective separation, the slope of the potential barrier between the split ion chains should be maximized. Assuming a fixed depth for both wells after splitting, this condition can be met by minimizing the distance between the wells’ minima (denoted as l l ).
To determine the optimal geometrical configuration for the smallest possible distance, we fix the depth of each potential well after separation to 0.05 0.05\, eV and vary the widths of the electrodes within the following ranges: r 0 ∈ [ 50,200 ] r_{0}\in[50,200]\, μ \mu m, r 1 ∈ [ 100,300 ] r_{1}\in[100,300]\, μ \mu m, r 2 ∈ [ 500,800 ] ​ μ r_{2}\in[500,800]\,\ \mu m. The parameters of the optimal configuration, yielding the distance of 350 μ \mu m between the wells’ minima, are presented in Table 1 . 
 r 0 r_{0} 
 r 1 r_{1} 
 r 2 r_{2} 
 l l 
 150 μ ​ m \mu m 
 300 μ ​ m \mu m 
 700 μ ​ m \mu m 
 350 μ ​ m \mu m 
 Таблица 1: Table 2. Parameters of optimal configuration for ion chain splitting with trap presented on Fig. 7 . 
 The proposed design of the surface Paul trap splitting element is easy to manufacture and allows to create a double-well potential in the z-direction of the trap geometry. By optimizing the lengths of the electrodes in the axial direction, we ensured the maximum slope of the central potential barrier. 
 6 Conclusion 
 In this study, we detailed the design process of a surface Paul trap optimized for confining and manipulating ytterbium ions. We began by calculating the distance from the ion to the trap surface and the potential depth, establishing a configuration that holds the ion at a height of h = 80 h=80 μ \mu m and can trap ions with energies below 110 110 meV. During optimization, we considered the stability parameter and the secular frequency of the trap, ensuring their values remained close to those proven effective in previous experiments. Subsequently, we adjusted the principal axes of secular motion by an angle of α = 14 ∘ \alpha=14^{\circ} by introducing asymmetry in the RF electrodes, achieving conditions for efficient laser cooling. Additionally, we equipped the trap with a feature for ion chain division using the outer DC electrodes to create a well-localized separating potential barrier. Moving forward, we aim to implement the calculated design on a microchip and conduct ion-trapping experiments. 
 Funding 
 This work is supported by RSF grant № 24-12-00415. 
 Conflict of interest 
 The authors of this work declare that they have no conflicts of interest. 
 Список литературы 
 [1] 
L. K. Grover, ‘‘Quantum computers can search arbitrarily large databases by a single query,’’ Physical Review Letters , vol. 79, pp. 4709–4712, 12 1997.
 [2] 
P. W. Shor, ‘‘Proceedings of the 35th annual symposium on foundations of computer science,’’ IEE Computer society press, Santa Fe, NM , 1994.
 [3] 
J. I. Cirac and P. Zoller, ‘‘Quantum computations with cold trapped ions,’’ Phys. Rev. Lett. , vol. 74, pp. 4091–4094, May 1995. [Online]. Available: https://link.aps.org/doi/10.1103/PhysRevLett.74.4091
 [4] 
W. Neuhauser, M. Hohenstatt, P. E. Toschek, and H. Dehmelt, ‘‘Localized visible ba+ mono-ion oscillator,’’ Phys. Rev. A , vol. 22, pp. 1137–1140, Sep 1980. [Online]. Available: https://link.aps.org/doi/10.1103/PhysRevA.22.1137
 [5] 
M. Spagnolo, J. Morris, S. Piacentini, M. Antesberger, F. Massa, A. Crespi, F. Ceccarelli, R. Osellame, and P. Walther, ‘‘Experimental photonic quantum memristor,’’ Nature Photonics , vol. 16, no. 4, pp. 318–323, 2022.
 [6] 
S. Stremoukhov, P. Forsh, K. Khabarova, and N. Kolachevsky, ‘‘Proposal for trapped-ion quantum memristor,’’ Entropy , vol. 25, no. 8, p. 1134, 2023.
 [7] 
S. Y. Stremoukhov, P. Forsh, K. Y. Khabarova, and N. Kolachevsky, ‘‘Model of coupled quantum memristors based on a single trapped 171yb+ ion,’’ JETP Letters , vol. 119, no. 5, pp. 352–356, 2024.
 [8] 
C. D. Bruzewicz, J. Chiaverini, R. McConnell, and J. M. Sage, ‘‘Trapped-ion quantum computing: Progress and challenges,’’ Applied Physics Reviews , vol. 6, no. 2, 2019.
 [9] 
G. Pagano, P. Hess, H. Kaplan, W. Tan, P. Richerme, P. Becker, A. Kyprianidis, J. Zhang, E. Birckelbaw, M. Hernandez et al. , ‘‘Cryogenic trapped-ion system for large scale quantum simulation,’’ Quantum Science and Technology , vol. 4, no. 1, p. 014004, 2018.
 [10] 
P. H. Leung and K. R. Brown, ‘‘Entangling an arbitrary pair of qubits in a long ion crystal,’’ Physical Review A , vol. 98, no. 3, p. 032318, 2018.
 [11] 
D. Kielpinski, C. Monroe, and D. J. Wineland, ‘‘Architecture for a large-scale ion-trap quantum computer,’’ Nature , vol. 417, no. 6890, pp. 709–711, 2002.
 [12] 
J. Britton, D. Leibfried, J. Beall, R. Blakestad, J. Wesenberg, and D. Wineland, ‘‘Scalable arrays of rf paul traps in degenerate si,’’ Applied Physics Letters , vol. 95, no. 17, 2009.
 [13] 
S. Seidelin, J. Chiaverini, R. Reichle, J. J. Bollinger, D. Leibfried, J. Britton, J. Wesenberg, R. Blakestad, R. Epstein, D. Hume et al. , ‘‘Microfabricated surface-electrode ion trap for scalable quantum information processing,’’ Physical review letters , vol. 96, no. 25, p. 253003, 2006.
 [14] 
M. D. Hughes, B. Lekitsch, J. A. Broersma, and W. K. Hensinger, ‘‘Microfabricated ion traps,’’ Contemporary Physics , vol. 52, no. 6, pp. 505–529, 2011.
 [15] 
Z. D. Romaszko, S. Hong, M. Siegele, R. K. Puddy, F. R. Lebrun-Gallagher, S. Weidt, and W. K. Hensinger, ‘‘Engineering of microfabricated ion traps and integration of advanced on-chip features,’’ Nature Reviews Physics , vol. 2, no. 6, pp. 285–299, 2020.
 [16] 
R. Sterling, M. Hughes, C. Mellor, and W. Hensinger, ‘‘Increased surface flashover voltage in microfabricated devices,’’ Applied Physics Letters , vol. 103, no. 14, 2013.
 [17] 
J. M. Wilson, J. N. Tilles, R. A. Haltli, E. Ou, M. G. Blain, S. M. Clark, and M. C. Revelle, ‘‘In situ detection of rf breakdown on microfabricated surface ion traps,’’ Journal of Applied Physics , vol. 131, no. 13, 2022.
 [18] 
J.-S. Chen, E. Nielsen, M. Ebert, V. Inlek, K. Wright, V. Chaplin, A. Maksymov, E. Páez, A. Poudel, P. Maunz et al. , ‘‘Benchmarking a trapped-ion quantum computer with 29 algorithmic qubits,’’ arXiv preprint arXiv:2308.05071 , 2023.
 [19] 
S. A. Moses, C. H. Baldwin, M. S. Allman, R. Ancona, L. Ascarrunz, C. Barnes, J. Bartolotta, B. Bjork, P. Blanchard, M. Bohn et al. , ‘‘A race-track trapped-ion quantum processor,’’ Physical Review X , vol. 13, no. 4, p. 041052, 2023.
 [20] 
D. Berkeland, J. Miller, J. C. Bergquist, W. M. Itano, and D. J. Wineland, ‘‘Minimization of ion micromotion in a paul trap,’’ Journal of applied physics , vol. 83, no. 10, pp. 5025–5033, 1998.
 [21] 
A. H. Nizamani and W. K. Hensinger, ‘‘Optimum electrode configurations for fast ion separation in microfabricated surface ion traps,’’ Applied Physics B , vol. 106, pp. 327–338, 2012.
 [22] 
M. House, ‘‘Analytic model for electrostatic fields in surface-electrode ion traps,’’ Physical Review A , vol. 78, no. 3, p. 033402, 2008.
 [23] 
S. Hong, M. Lee, H. Cheon, T. Kim, and D.-i. D. Cho, ‘‘Guidelines for designing surface ion traps using the boundary element method,’’ Sensors , vol. 16, no. 5, p. 616, 2016.
 [24] 
T. Abbasov, S. Zibrov, and I. Sherstov, ‘‘Surface-electrode ion trap development,’’ JETP Letters , vol. 118, no. 3, pp. 215–219, 2023.
 [25] 
C. Ryan-Anderson, N. Brown, M. Allman, B. Arkin, G. Asa-Attuah, C. Baldwin, J. Berg, J. Bohnet, S. Braxton, N. Burdick et al. , ‘‘Implementing fault-tolerant entangling gates on the five-qubit code and the color code,’’ arXiv preprint arXiv:2208.01863 , 2022.
 [26] 
P. Wang, C.-Y. Luan, M. Qiao, M. Um, J. Zhang, Y. Wang, X. Yuan, M. Gu, J. Zhang, and K. Kim, ‘‘Single ion qubit with estimated coherence time exceeding one hour,’’ Nature communications , vol. 12, no. 1, p. 233, 2021.
 [27] 
M. Aksenov, I. Zalivako, I. Semerikov, A. Borisenko, N. Semenin, P. Sidorov, A. Fedorov, K. Y. Khabarova, and N. Kolachevsky, ‘‘Realizing quantum gates with optically addressable Yb + 171 {}^{171}\mathrm{Yb}^{+} ion qudits,’’ Physical Review A , vol. 107, no. 5, p. 052612, 2023.
 [28] 
I. V. Zalivako, A. S. Borisenko, I. A. Semerikov, A. E. Korolkov, P. L. Sidorov, K. P. Galstyan, N. V. Semenin, V. N. Smirnov, M. D. Aksenov, A. K. Fedorov et al. , ‘‘Continuous dynamical decoupling of optical Yb + 171 {}^{171}\mathrm{Yb}^{+} qudits with radiofrequency fields,’’ Frontiers in Quantum Science and Technology , vol. 2, p. 1228208, 2023.
 [29] 
I. V. Zalivako, A. S. Nikolaeva, A. S. Borisenko, A. E. Korolkov, P. L. Sidorov, K. P. Galstyan, N. V. Semenin, V. N. Smirnov, M. A. Aksenov, K. M. Makushin et al. , ‘‘Towards multiqudit quantum processor based on a Yb + 171 {}^{171}\mathrm{Yb}^{+} ion string: Realizing basic quantum algorithms,’’ arXiv preprint arXiv:2402.03121 , 2024.
 [30] 
A. S. Kazmina, I. V. Zalivako, A. S. Borisenko, N. A. Nemkov, A. S. Nikolaeva, I. A. Simakov, A. V. Kuznetsova, E. Y. Egorova, K. P. Galstyan, N. V. Semenin et al. , ‘‘Demonstration of a parity-time-symmetry-breaking phase transition using superconducting and trapped-ion qutrits,’’ Physical Review A , vol. 109, no. 3, p. 032619, 2024.
 [31] 
M. Niedermayr, ‘‘Cryogenic surface ion traps,’’ PhD. Universität Innsbruck , p. 83, 2015.
 [32] 
J. J. McLoughlin, A. H. Nizamani, J. D. Siverns, R. C. Sterling, M. D. Hughes, B. Lekitsch, B. Stein, S. Weidt, and W. K. Hensinger, ‘‘Versatile ytterbium ion trap experiment for operation of scalable ion-trap chips with motional heating and transition-frequency measurements,’’ Physical Review A , vol. 83, no. 1, p. 013406, 2011.
 [33] 
D. Kiesenhofer, H. Hainzer, A. Zhdanov, P. C. Holz, M. Bock, T. Ollikainen, and C. F. Roos, ‘‘Controlling two-dimensional coulomb crystals of more than 100 ions in a monolithic radio-frequency trap,’’ PRX Quantum , vol. 4, no. 2, p. 020317, 2023.
 [34] 
D. Leibfried, R. Blatt, C. Monroe, and D. Wineland, ‘‘Quantum dynamics of single trapped ions,’’ Reviews of Modern Physics , vol. 75, no. 1, p. 281, 2003.
 [35] 
M. Brownnutt, M. Kumph, P. Rabl, and R. Blatt, ‘‘Ion-trap measurements of electric-field noise near surfaces,’’ Reviews of modern Physics , vol. 87, no. 4, p. 1419, 2015.
 [36] 
https://github.com/nist-ionstorage/electrode/.
 Experimental support, please
 view the build logs 
 for errors. Generated by
 L
 A 
 T
 E 
 xml 
 .
 Instructions for reporting errors 
 We are continuing to improve HTML versions of papers, and your feedback helps enhance accessibility and mobile
 support. To report errors in the HTML that will help us improve conversion and rendering, choose any of the
 methods listed below: 
 Click the "Report Issue" ( 
 ) button, located in the page header. 
 Tip: You can select the relevant text first, to include it in your report. 
 Our team has already identified the following issues . We appreciate your time reviewing and reporting rendering errors we
 may not have found yet. Your efforts will help us improve the HTML versions for all readers, because disability
 should not be a barrier to accessing research. Thank you for your continued support in championing open access for
 all. 
 Have a free development cycle? Help support accessibility at arXiv! Our collaborators at LaTeXML maintain a list of packages that need conversion , and welcome developer contributions . 
 We gratefully acknowledge support from
 our major funders ,
 member institutions , ,
 and all contributors.
 About 
 · 
 Help 
 · 
 Contact 
 · 
 Subscribe 
 · 
 Copyright 
 · 
 Privacy 
 · 
 Accessibility 
 · 
 Operational Status (opens in new tab) 
 Major funding support from