Google Open-Sources Agent2Agent Protocol for Agentic Collaboration - InfoQ 
 BT 
 InfoQ Software Architects' Newsletter 
 A monthly overview of things you need to know as an architect or aspiring architect. 
 View an example 
 Enter your e-mail address 
 Select your country 
 Select a country 
 I consent to InfoQ.com handling my data as explained in this Privacy Notice . 
 We protect your privacy. 
 Close 
 Toggle Navigation 
 Facilitating the Spread of Knowledge and Innovation in Professional Software Development
 English edition 
 English edition 
 Chinese edition 
 Japanese edition 
 French edition 
 Write for InfoQ
 Search 
 Register 
 Sign in 
 Unlock the full InfoQ experience 
 Unlock the full InfoQ experience by logging in! Stay updated with your favorite authors and topics, engage with content, and download exclusive resources. 
 Log In 
 or 
 Don't have an InfoQ account? 
 Register 
 Stay updated on topics and peers that matter to you Receive instant alerts on the latest insights and trends. 
 Quickly access free resources for continuous learning Minibooks, videos with transcripts, and training materials. 
 Save articles and read at anytime Bookmark articles to read whenever youre ready. 
 Logo - Back to homepage
 News 
 Articles 
 Presentations 
 Podcasts 
 Guides 
 Topics 
 Development 
 Java 
 Kotlin 
 .Net 
 C# 
 Swift 
 Go 
 Rust 
 JavaScript 
 Featured in Development 
 Adopting Memory-Safety and Fine-Grained Compartmentalisation with CHERI 
 David Chisnall discusses how the CHERI hardware architecture redefines pointer safety to solve isolation and sharing challenges. He explains how CHERI enables spatial and temporal memory safety for C/C++, scales down to microcontrollers with CHERIoT, and replaces costly OS-level RPC mechanisms with lightweight, auditable compartmentalization - all without requiring massive codebase rewrites. 
 All in development 
 Architecture & Design 
 Architecture 
 Enterprise Architecture 
 Scalability/Performance 
 Design 
 Case Studies 
 Microservices 
 Service Mesh 
 Patterns 
 Security 
 Featured in Architecture & Design 
 Agentic Fitness Functions: Extending Evolutionary Architecture Beyond Deterministic Rules 
 Deterministic rules safeguard hard metrics, but what about architectural intent? Discover how agentic fitness functions combine AI agents and versioned rubrics to evaluate complex, judgment-heavy concerns—such as boundary fidelity, semantic contract drift, and stale ADR assumptions. Elevate evolutionary architecture governance with continuous, calibrated feedback loops. 
 All in architecture-design 
 AI Infrastructure 
 Big Data 
 Machine Learning 
 NoSQL 
 Database 
 Data Analytics 
 Streaming 
 Featured in AI, ML & Data Engineering 
 From Fab To Token - The State Of The Market 
 Jordan Nanos discusses how semiconductor constraints, data center expansion, and networking bottlenecks impact AI software architecture. Drawing from SemiAnalysis research, he shares insights on benchmark performance, GPU scaling, and tokenomics from chip fab to model inference. 
 All in ai-ml-data-eng 
 Culture & Methods 
 Agile 
 Diversity 
 Leadership 
 Lean/Kanban 
 Personal Growth 
 Scrum 
 Sociocracy 
 Software Craftmanship 
 Team Collaboration 
 Testing 
 UX 
 Featured in Culture & Methods 
 Turning Outward: Growing From Code to Influence 
 Brad Grantham discusses how software engineers and architects can transition from individual contributors to influential technical leaders. Brad shares actionable insights on expanding skills into business and legal domains, adapting communication styles for non-technical stakeholders, moving past ego to empower teams, and navigating complex organizational dynamics to maximize engineering impact. 
 All in culture-methods 
 DevOps 
 Infrastructure 
 Continuous Delivery 
 Automation 
 Containers 
 Cloud 
 Observability 
 Featured in DevOps 
 Platform Engineering for Everyone - Success Can’t Be Coded 
 Max Korbacher explains why successful internal development platforms cannot be built on tech alone. He discusses the pitfalls of infrastructure-first thinking, the importance of a clear product mindset, and how to measure real value using DevEx and SPACE metrics. Learn how to align your team, manage tech debt, and foster a thriving community to ensure lasting platform adoption. 
 All in devops 
 Events 
 Helpful links 
 About InfoQ
 InfoQ Editors
 Write for InfoQ
 About C4Media
 Diversity 
 Choose your language 
 En 
 中文 
 日本 
 Fr 
 Aug 26, 2026 
 AI Security & Privacy Engineering Certification 
 Secure and govern production AI systems, from sensitive data to guardrails, evals, and audits. Online. Register now. 
 Sep 14, 2026 
 Architect Certification 
 Distributed systems, decentralized decisions, platform engineering, and AI architecture. Online. Register Now. 
 Sep 18, 2026 
 Engineering Leadership Certification 
 Work through leadership decisions with senior peers facing similar technical trade-offs. Online. Register Now. 
 Sep 18, 2026 
 AI-Assisted Engineering Certification 
 Your coding agent moves fast. Who checks whether it's making the codebase better? Online. Register Now. 
 Nov 16-20, 2026 
 QCon San Francisco 
 What's working across AI, architecture, and leadership, from the teams doing it. Register. Early bird ends August 11. 
 Dec 15-16, 2026 
 QCon AI New York 
 Production AI across agents, context, evals, security, and infrastructure, from the senior engineers building it. Registration open. 
 Apr 13-16, 2027 
 QCon London 
 What early-adopter teams have proven in production, across 15 engineering tracks. Register. Early bird ends August 11. 
 InfoQ Homepage 
 News 
 Google Open-Sources Agent2Agent Protocol for Agentic Collaboration 
 AI, ML & Data Engineering
 Google Open-Sources Agent2Agent Protocol for Agentic Collaboration
 Apr 15, 2025 
 2
 min read
 by 
 Anthony Alford 
 Follow us on 
 Youtube 232K Followers 
 Linkedin 26K Followers 
 Instagram New 
 RSS 19K Readers 
 X 57.1k Followers 
 Facebook 21K Likes 
 Bluesky New 
 Listen to this article -  0:00 
 Audio ready to play 
 Your browser does not support the audio element.
 0:00 
 0:00 
 Normal 1.25x 1.5x 
 Like 
 Reading list 
 Google released the Agent2Agent (A2A) Protocol , an open-source specification for building AI agents that can connect with other agents that support the protocol. Google has enlisted over 50 technology partners to contribute to A2A's development. 
 Google announced the release at the recent Google Cloud Next conference . A2A is billed as a "complement" to Anthropic's Model Context Protocol (MCP) and defines a client-server relationship between AI agents. Google developed the protocol with help from partners like Salesforce , Atlassian , and LangChain , with the goal of creating an interoperability standard for any agent, regardless of vendor or framework. According to Google, 
 A2A has the potential to unlock a new era of agent interoperability, fostering innovation and creating more powerful and versatile agentic systems. We believe that this protocol will pave the way for a future where agents can seamlessly collaborate to solve complex problems and enhance our lives. We’re committed to building the protocol in collaboration with our partners and the community in the open. We’re releasing the protocol as open source and setting up clear pathways for contribution. 
 InfoQ covered Anthropic's MCP release last year. Intended to solve the "MxN" problem---the combinatorial difficulty of integrating M different LLMs with N different tools---MCP defines a client-server architecture and a standard protocol that LLM vendors and tool builders can follow. 
 Google's documentation points out that A2A solves a different problem than MCP does: it "allows agents to communicate as agents (or as users) instead of as tools." The difference between a tool and an agent is that tools have structured I/O and behavior, while agents are autonomous and can solve new tasks using reasoning. In Google's vision, an agentic application requires both tools and agents. However, A2A docs do recommend that "applications model A2A agents as MCP resources." 
 A2A defines three types of actor : remote agents , which are "blackbox" agents on an A2A server; clients that request action from remote servers; and users (human users or services) that want to accomplish tasks using an agentic system. Like MCP, A2A uses JSON-RPC over HTTP for communication between clients and remote agents. The core abstraction used in the communication spec between agents is the task , which is created by a client and fulfilled by a remote agent. 
 In a Hacker News discussion, several users compared A2A to MCP ; some were not sure what value A2A proved over MCP, while others saw it as a "superset" of MCP and praised its "clear documentation and explanation" compared to MCP. User TS_Posts claimed to be working on A2A and wrote: 
 [T]he current specification and samples are early. We are working on many more advanced examples and official SDKs and client/servers. We're working with partners, other Google teams, and framework providers to turn this into a stable standard. We're doing it in the open - so there are things that are missing because (a) it's early and (b) we want partners and the community to bring features to the table. tldr - this is NOT done. We want your feedback and sincerely appreciate it! 
 The A2A source code is available on GitHub. Google also released a demo video showing collaboration between agents from different frameworks. 
 About the Author 
 Anthony Alford 
 Show more Show less 
 Rate this Article 
 Adoption 
 Style 
 Author Contacted 
 This content is in the AI, ML & Data Engineering topic
 Related Topics: 
 AI, ML & Data Engineering 
 Google 
 Agents 
 Related Editorial 
 Related Sponsors 
 Popular across InfoQ 
 Netflix Adopts Cloud-Native Job Queueing System Kueue to Replace an In-House Solution
 Cloudflare Migrates JavaScript CDN Serving 9B Requests a Day to Its Developer Platform
 MCP Goes Stateless, and Developers Ask Whether That Just Makes it an API Again
 How PGSimCity Turns PostgreSQL Complexity into a Virtual City 3D Simulation
 Astro 7: Rust Compiler, Rust Markdown Pipeline and Vite 8 for Builds Up to 61% Faster
 Cloud and DevOps InfoQ Trends Report 2026: AI, Resilience, Platforms, FinOps, and Sovereignty
 Related Content 
 The InfoQ Newsletter 
 A round-up of last week’s content on InfoQ sent out every Tuesday. Join a community of over 250,000 senior developers.
 View an example 
 Enter your e-mail address 
 Select your country 
 Select a country 
 I consent to InfoQ.com handling my data as explained in this Privacy Notice . 
 We protect your privacy. 
 Development 
 How PGSimCity Turns PostgreSQL Complexity into a Virtual City 3D Simulation 
 LLM-Generated GraphQL Mocks Arrive at Airbnb and Expedia, While the Spec Lags behind 
 Adopting Memory-Safety and Fine-Grained Compartmentalisation with CHERI 
 Architecture & Design 
 Grab Cuts Mechanical Analytics Work From 44% to 30% with AI Agents 
 Agentic Fitness Functions: Extending Evolutionary Architecture Beyond Deterministic Rules 
 Will Agentic AI Bring Fantasia’s Sorcerer's Apprentice to Life?: A Conversation with Tracy Bannon 
 Culture & Methods 
 Turning Outward: Growing From Code to Influence 
 Founders, Friction, and Focus: Building Engineering Teams at Early-Stage Startups 
 How Artificial Intelligence Disrupts Engineering Progression 
 AI, ML & Data Engineering 
 From Fab To Token - The State Of The Market 
 Netflix Open-Sources Agentic Workflow for Causal Inference 
 Major Frontier Model Providers Adopt Watermarking Tech to Comply with EU Regulation 
 DevOps 
 GitHub Brings Stacked Pull Requests to Public Preview 
 Cloudflare Turns CI Pipelines into TypeScript Workflows 
 Grafana's gcx and MCP Server Reach GA for Telemetry-Driven Agent Development 
 The InfoQ Newsletter
 A round-up of last week’s content on InfoQ sent out every Tuesday. Join a community of over 250,000 senior developers.
 View an example 
 Get a quick overview of content published on a variety of innovator and early adopter technologies 
 Learn what you don’t know that you don’t know 
 Stay up to date with the latest information from the topics you are interested in 
 Enter your e-mail address 
 Select your country 
 Select a country 
 I consent to InfoQ.com handling my data as explained in this Privacy Notice . 
 We protect your privacy. 
 InfoQ Online Certification Programs 
 For Senior Engineers, Architects, and Technical Leaders
 AI Security & Privacy Engineering with Katharine Jarmul | August 26 
 Architect with Luca Mezzalira | September 14 
 Engineering Leadership with Michelle Brush | September 18 
 As your role becomes more senior, the work changes. You're no longer just implementing decisions; you're shaping the systems, trade-offs, and technical direction other teams depend on. These 5-week online programs give you a structured way to work through real decisions from your current role with senior peers from other companies. 
 RESERVE YOUR PLACE 
 Live online. 4 hours a week, for 5 weeks. 
 Home 
 Create account 
 Log In 
 QCon Conferences 
 Events 
 Write for InfoQ 
 InfoQ Editors 
 About InfoQ 
 About C4Media 
 Media Kit
 InfoQ Developer Marketing Blog 
 Diversity 
 Events 
 Online InfoQ AI Security & Privacy Engineering Program 
 August 26, 2026 
 Online InfoQ Architect Certification 
 September 14, 2026 
 Online InfoQ Engineering Leadership Certification 
 September 18, 2026 
 Online InfoQ AI-Assisted Engineering Certification 
 September 18, 2026 
 QCon San Francisco 
 November 16-20, 2026 
 QCon AI New York 
 December 15-16, 2026 
 QCon London 2027 
 April 13-16, 2027 
 Follow us on 
 Youtube 232K Followers 
 Linkedin 26K Followers 
 Instagram New 
 RSS 19K Readers 
 X 57.1k Followers 
 Facebook 21K Likes 
 Bluesky New 
 Stay in the know 
 The InfoQ Podcast 
 Engineering Culture Podcast 
 The Software Architects' Newsletter 
 General Feedback
 feedback@infoq.com 
 Advertising
 sales@infoq.com 
 Editorial
 editors@infoq.com 
 Marketing
 marketing@infoq.com 
 InfoQ.com and all content copyright © 2006-2026 C4Media Inc. 
 Privacy Notice , Terms And Conditions , Cookie Policy 
 BT