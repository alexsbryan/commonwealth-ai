<img src="https://r2cdn.perplexity.ai/pplx-full-logo-primary-dark%402x.png" class="logo" width="120"/>

# Solving Asymmetric First-Price Auctions: Methods and Challenges

First-price sealed-bid auctions with asymmetric bidders present significant analytical challenges compared to their symmetric counterparts. When bidders draw values from different distributions, closed-form solutions rarely exist, necessitating sophisticated numerical approaches. This report examines the primary methods used to solve such auctions with two asymmetric bidders.

## Differential Equation Formulation

The standard approach to solving asymmetric first-price auctions involves formulating and solving a system of differential equations that characterize bidders' equilibrium strategies.

### Mathematical Foundation

For a two-bidder case where bidders have values drawn from distributions F₁ and F₂, the equilibrium is typically characterized by a system of differential equations for the inverse bid functions λ₁(b) and λ₂(b)[^1_1][^1_2]. These equations take the form:

\$ λ₁'(b) = \frac{(λ₁(b) - b)f₂(λ₂(b))}{F₂(λ₂(b))} \$

\$ λ₂'(b) = \frac{(λ₂(b) - b)f₁(λ₁(b))}{F₁(λ₁(b))} \$

Where f₁ and f₂ are the density functions corresponding to F₁ and F₂[^1_2]. This system is subject to appropriate boundary conditions, typically set at the common minimum bid[^1_1].

### Boundary Conditions

Setting proper boundary conditions is crucial and requires careful consideration[^1_9]. For two asymmetric bidders with value distributions having supports [c₁,d₁] and [c₂,d₂], the appropriate boundary conditions depend on the relationship between these supports[^1_3].

When supports are identical, both bidders have the same maximum bid at equilibrium[^1_14]. However, when supports differ, determining the correct boundary conditions becomes more complex and may involve "bid bifurcation" where bidding supports do not fully overlap[^1_9].

## Numerical Solution Methods

### Backward-Shooting Method

The most widely used numerical approach is the backward-shooting method, introduced by Marshall et al. (1994)[^1_6]. This method:

1. Sets initial conditions at the upper bound of the bid space
2. Integrates the differential equations backward toward the lower bound
3. Adjusts the initial conditions iteratively until boundary conditions are satisfied

While widely used, this method suffers from inherent instability, particularly near the lower bound of the bid range[^1_6]. As noted by Marshall et al., "backward solutions are well-behaved except in neighborhoods of the origin where they become (highly) unstable"[^1_6].

### Taylor Series Expansions

Gayle and Richard proposed a more robust algorithm using local Taylor series expansions of both the solution and the distributions[^1_1][^1_6]. Their approach:

1. Builds an algebra of local Taylor-series expansions
2. Computes highly accurate solutions to the differential equations
3. Includes automatic procedures for generating expansions for arbitrary distributions
4. Calculates auxiliary statistics like expected revenues and winning probabilities

This method offers improved numerical stability and accuracy, especially for complex distribution specifications[^1_1].

### Software Implementation

Several software implementations exist for these numerical methods:

1. **BIDCOMP2** by Li and Riley - A freely available implementation using adaptive step sizes for numerical backward integration[^1_6]
2. **Algorithm by Gayle and Richard** - Implements Taylor series expansions and offers extended functionality including coalition analysis[^1_1][^1_4]

## Alternative Analytical Approaches

### Analysis via Winning Probabilities

Kirkegaard proposes examining winning probabilities rather than directly analyzing bidding strategies[^1_14]. This approach:

1. Exploits the connection between winning probabilities and payoffs from mechanism design
2. Circumvents the complexity of directly analyzing bidding strategies
3. Makes many existing results easier to prove and generates new insights
4. Applies to both first-price and all-pay auctions

This method has proven particularly useful for analyzing types of asymmetry not easily addressed by traditional approaches[^1_14].

### Mechanism Design Approach

Some researchers have applied mechanism design principles to analyze asymmetric auctions without directly solving for equilibrium strategies[^1_7]. This approach has been used to compare revenue across different auction formats and analyze efficiency implications[^1_8].

## Properties of Equilibrium Solutions

Several important properties characterize solutions to asymmetric first-price auctions:

1. **Existence and Uniqueness**: Under standard regularity conditions, an equilibrium exists and is unique when valuation distributions have a mass point at the lower extremity or meet specific technical conditions[^1_2][^1_10].
2. **Bidding Behavior**: When one bidder is "stronger" (in terms of first-order stochastic dominance), they typically shade their bids more than the weaker bidder, resulting in inefficient allocations[^1_13][^1_8].
3. **No Consistent Dominance**: As established by Lebrun, it's impossible for one bidder to win consistently more often than another bidder across all valuation levels[^1_14].
4. **Boundary Behavior**: Both bidders with the lowest possible valuation bid the same amount, and bidders with the highest valuation in the common support bid the same amount[^1_14].

## Conclusion

While no general closed-form solution exists for asymmetric first-price auctions, robust numerical methods have been developed to compute equilibrium strategies. The differential equation approach, particularly when implemented via Taylor series expansions or modified backward-shooting algorithms, offers a powerful framework for analysis.

For researchers and practitioners dealing with asymmetric auctions, the choice of solution method depends on the specific distributions involved and the required accuracy. Taylor series methods offer better numerical stability, while alternative analytical approaches can provide insights without full numerical solutions in some cases.

As auction theory continues to develop, more efficient numerical methods and novel analytical approaches may emerge to further simplify the analysis of these complex strategic interactions.

<div style="text-align: center">⁂</div>

[^1_1]: https://capcp.la.psu.edu/wp-content/uploads/sites/11/2020/07/NumericalSolutions.pdf

[^1_2]: https://blebrun.info.yorku.ca/files/2016/05/FPANB-DP97.pdf?x20523

[^1_3]: https://www.isid.ac.in/~epu/acegd2022/papers/G_V_A_Dharanan.pdf

[^1_4]: https://capcp.la.psu.edu/wp-content/uploads/sites/11/numericalanalysis.pdf

[^1_5]: https://math.stackexchange.com/questions/1385728/system-of-differential-equations-asymmetric-first-price-auction

[^1_6]: http://www.math.tau.ac.il/~fibich/Manuscripts/Numerical-simulations-of-asymmetric-first-price-auctions.pdf

[^1_7]: https://www.econometricsociety.org/uploads/Supmat/9859_extensions.pdf

[^1_8]: https://kylewoodward.com/blog-data/pdfs/references/kirkegaard-journal-of-economic-theory-2009A.pdf

[^1_9]: https://www.uoguelph.ca/economics/repec/workingpapers/2015/2015-02.pdf

[^1_10]: https://econ.laps.yorku.ca/files/2015/10/lebrunb-u.pdf

[^1_11]: https://capcp.la.psu.edu/wp-content/uploads/sites/11/Working Papers/2007/AsymResale.pdf

[^1_12]: https://scholar.harvard.edu/files/maskin/files/asymmetric_auctions.pdf

[^1_13]: https://users.ssc.wisc.edu/~dquint/econ805 2007/econ 805 lecture 9.pdf

[^1_14]: https://brocku.ca/repec/pdf/0504.pdf

[^1_15]: https://economics.uwo.ca/people/zheng_docs/1stpriceasym.pdf

[^1_16]: https://www.sciencedirect.com/science/article/pii/S0022053109000295

[^1_17]: https://www.jstor.org/stable/4127011

[^1_18]: http://www.econ.ucla.edu/riley/research/asyRES.PDF

[^1_19]: https://www.sciencedirect.com/science/article/pii/S0899825624000848

[^1_20]: https://pubsonline.informs.org/doi/10.1287/deca.2021.0432

[^1_21]: https://www.sciencedirect.com/science/article/pii/S0899825697906357

[^1_22]: https://www.degruyter.com/document/doi/10.1515/bejte-2016-0196/html?lang=en

[^1_23]: https://en.wikipedia.org/wiki/Auction_theory

[^1_24]: https://www.sciencedirect.com/science/article/pii/S0899825611000509

[^1_25]: https://onlinelibrary.wiley.com/doi/abs/10.1111/1468-2354.00008

[^1_26]: https://www.cirje.e.u-tokyo.ac.jp/research/workshops/micro/documents/March20.pdf

[^1_27]: https://www.jstor.org/stable/2648842

[^1_28]: https://econ.laps.yorku.ca/files/2015/10/lebrunb-u.pdf

[^1_29]: http://www.math.tau.ac.il/~fibich/Manuscripts/Numerical-simulations-of-asymmetric-first-price-auctions.pdf

[^1_30]: https://www.ssc.wisc.edu/~dquint/econ805 2007/econ 805 lecture 9.pdf

[^1_31]: https://scholar.harvard.edu/files/maskin/files/asymmetric_auctions.pdf

[^1_32]: https://brocku.ca/repec/pdf/0504.pdf

[^1_33]: https://pubsonline.informs.org/doi/10.1287/moor.1110.0535

[^1_34]: http://repec.org/sce2005/up.18137.1108489875.pdf?origin=publication_detail

[^1_35]: https://math.stackexchange.com/questions/1385728/system-of-differential-equations-asymmetric-first-price-auction

[^1_36]: https://www.sciencedirect.com/science/article/pii/S0899825605000540

[^1_37]: https://www.sciencedirect.com/science/article/abs/pii/S0899825611000509

[^1_38]: https://scholar.harvard.edu/files/maskin/files/equilibrium_in_sealed_high_bid_auctions.pdf

[^1_39]: https://eml.berkeley.edu/~mcfadden/eC103_f03/ps9sol1210.pdf

[^1_40]: https://www.degruyter.com/document/doi/10.2202/1534-5971.1304/pdf

[^1_41]: https://faculty.engineering.asu.edu/bertsekas/wp-content/uploads/sites/129/2019/10/Reverse-Auction-and-the-Solution-of-Asymmetric-Assignment-Problems.pdf

[^1_42]: https://papers.ssrn.com/sol3/Delivery.cfm/SSRN_ID3475679_code1152163.pdf?abstractid=3475679\&mirid=1

[^1_43]: https://epubs.siam.org/doi/10.1137/0511057

[^1_44]: https://econen.sufe.edu.cn/_upload/article/files/9d/6d/74a232d54cecbfac670c74f859f2/b4ef667f-6377-4244-991a-7ca360d834f1.pdf

[^1_45]: https://en.wikipedia.org/wiki/Finite_difference

[^1_46]: https://www.sciencedirect.com/science/article/abs/pii/S0899825622000070

[^1_47]: https://pythonnumericalmethods.berkeley.edu/notebooks/chapter20.02-Finite-Difference-Approximating-Derivatives.html

[^1_48]: https://economics.utoronto.ca/conferences/index.php/cetc/2016/paper/view/918/352

[^1_49]: https://capcp.la.psu.edu/wp-content/uploads/sites/11/numericalanalysis.pdf

[^1_50]: https://ideas.repec.org/p/lvl/laeccr/9715.html

[^1_51]: https://onlinelibrary.wiley.com/doi/pdf/10.1111/1468-2354.00008

[^1_52]: https://kylewoodward.com/blog-data/pdfs/references/kirkegaard-journal-of-economic-theory-2009A.pdf

[^1_53]: https://ideas.repec.org/a/ier/iecrev/v40y1999i1p125-42.html

[^1_54]: https://www.sciencedirect.com/science/article/abs/pii/S0165176511002473

[^1_55]: https://www.aeaweb.org/articles/materials/324

[^1_56]: https://pubs.aeaweb.org/doi/10.1257/aer.98.1.87

[^1_57]: https://papers.ssrn.com/sol3/papers.cfm?abstract_id=3475679

[^1_58]: https://blebrun.info.yorku.ca/files/2016/05/FPANB-DP97.pdf?x20523

[^1_59]: https://www.uoguelph.ca/economics/repec/workingpapers/2015/2015-02.pdf

[^1_60]: https://www.asc.ohio-state.edu/ye.45/Econ816/Hafalir-Vijay.pdf

[^1_61]: https://eprints.ugd.edu.mk/23041/1/dushko et.all-147-172.pdf

[^1_62]: https://www.weiran-shen.info/page_files/bayesian_nash_equilibrium_in_first-price_auction_with_discrete_value_distributions.pdf

[^1_63]: https://www.econometricsociety.org/uploads/Supmat/9859_extensions.pdf

[^1_64]: http://www.renekirkegaard.com/s/EfficiencyWithReserves21.pdf

