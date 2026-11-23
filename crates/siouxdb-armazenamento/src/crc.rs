//! CRC32C (Castagnoli), o mesmo usado por iSCSI, ext4 e outros bancos.
//! Implementação por tabela, sem dependências.

const POLINOMIO: u32 = 0x82F6_3B78;

const TABELA: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ POLINOMIO
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC32C dos dados.
pub fn crc32c(dados: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in dados {
        c = TABELA[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

#[cfg(test)]
mod testes {
    use super::crc32c;

    #[test]
    fn vetores_conhecidos() {
        // Vetor padrão de conferência do CRC-32C.
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
        // RFC 3720 (iSCSI): 32 bytes zerados.
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
    }
}
