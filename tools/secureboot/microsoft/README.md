# Microsoft's Secure Boot certificates

Public certificates, enrolled beside hideOS's own keys by the installer (see
ARCHITECTURE.md, "Security"). Downloaded from the links Microsoft gives in
"Windows Secure Boot Key Creation and Management Guidance"; DER, as served.

| File | Certificate | Enrolled in | Why |
|---|---|---|---|
| `kek-2011.der` | Microsoft Corporation KEK CA 2011 | KEK | Microsoft's db and dbx updates — revocations — keep applying |
| `kek-2023.der` | Microsoft Corporation KEK 2K CA 2023 | KEK | The same, for its 2023 keys |
| `uefi-ca-2011.der` | Microsoft Corporation UEFI CA 2011 | db | Option ROMs: a GPU's or a network card's firmware |
| `uefi-ca-2023.der` | Microsoft UEFI CA 2023 | db | The same, from 2023 |
| `option-rom-ca-2023.der` | Microsoft Option ROM UEFI CA 2023 | db | Option ROMs, the 2023 CA for them alone |
| `windows-pca-2011.der` | Microsoft Windows Production PCA 2011 | db | Windows, on a machine that dual-boots |
| `windows-uefi-ca-2023.der` | Windows UEFI CA 2023 | db | The same, from 2023 |

The UEFI CAs also sign other distributions' shims: with them in db, a
Microsoft-signed boot loader boots too. That is the price of option ROMs
that work, the same one `sbctl enroll-keys --microsoft` pays.

```
a1117f516a32cefcba3f2d1ace10a87972fd6bbe8fe0d0b996e09e65d802a503  kek-2011.der           https://go.microsoft.com/fwlink/?LinkId=321185
3cd3f0309edae228767a976dd40d9f4affc4fbd5218f2e8cc3c9dd97e8ac6f9d  kek-2023.der           https://go.microsoft.com/fwlink/?linkid=2239775
48e99b991f57fc52f76149599bff0a58c47154229b9f8d603ac40d3500248507  uefi-ca-2011.der       https://go.microsoft.com/fwlink/?LinkId=321194
f6124e34125bee3fe6d79a574eaa7b91c0e7bd9d929c1a321178efd611dad901  uefi-ca-2023.der       https://go.microsoft.com/fwlink/?linkid=2239872
e5be3e64c6e66a281457ecdece0d6d0787577aad2a3a0144262c10c14ba8d8f1  option-rom-ca-2023.der https://go.microsoft.com/fwlink/?linkid=2284009
e8e95f0733a55e8bad7be0a1413ee23c51fcea64b3c8fa6a786935fddcc71961  windows-pca-2011.der   https://go.microsoft.com/fwlink/?linkid=321192
076f1fea90ac29155ebf77c17682f75f1fdd1be196da302dc8461e350a9ae330  windows-uefi-ca-2023.der https://go.microsoft.com/fwlink/?linkid=2239776
```
