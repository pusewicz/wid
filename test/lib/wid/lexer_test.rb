# frozen_string_literal: true

module Wid
  class LexerTest < Test
    def test_tokenize
      tokens = Lexer.tokenize(<<~WID)
        1 + 2.0
      WID

      assert_equal([:NUMBER, :+, :NUMBER, :"\n", :EOF], tokens.map(&:type))
    end

    def test_new_line
      tokens = Lexer.tokenize("1 + 2\n3\n")

      assert_equal([:NUMBER, :+, :NUMBER, :"\n", :NUMBER, :"\n", :EOF], tokens.map(&:type))
      assert_equal(0, tokens[0].line)
      assert_equal(0, tokens[1].line)
      assert_equal(0, tokens[2].line)
      assert_equal(0, tokens[3].line)

      assert_equal(1, tokens[4].line)
      assert_equal(1, tokens[5].line)

      assert_equal(2, tokens[6].line)
    end

    def test_position
      tokens = Lexer.tokenize("1 + 2\n       3  + 17\nputs(foo)    \n")

      assert_equal(0, tokens[0].column) # 1
      assert_equal(2, tokens[1].column) # +
      assert_equal(4, tokens[2].column) # 2
      assert_equal(5, tokens[3].column) # \n
      assert_equal(7, tokens[4].column) # 3
      assert_equal(10, tokens[5].column) # +
      assert_equal(12, tokens[6].column) # 17
      assert_equal(14, tokens[7].column) # \n
      assert_equal(0, tokens[8].column) # puts
      assert_equal(4, tokens[9].column) # (
      assert_equal(5, tokens[10].column) # foo
      assert_equal(8, tokens[11].column) # )
      assert_equal(13, tokens[12].column) # \n
    end

    def test_identifier
      tokens = Lexer.tokenize(<<~WID)
        foo
      WID

      assert_equal([:IDENTIFIER, :"\n", :EOF], tokens.map(&:type))
    end

    def test_keywords
      tokens = Lexer.tokenize(<<~WID)
        true false nil def end
      WID

      assert_equal([:TRUE, :FALSE, :NIL, :DEF, :END, :"\n", :EOF], tokens.map(&:type))
    end

    def test_punctuation
      tokens = Lexer.tokenize(<<~WID)
        { } ( ) [ ] = ! | & + - .
      WID

      assert_equal([:"{", :"}", :"(", :")", :"[", :"]", :"=", :!, :|, :&, :+, :-, :DOT, :"\n", :EOF], tokens.map(&:type))
    end

    def test_var_assignment
      tokens = Lexer.tokenize(<<~WID)
        foo = 1
      WID

      assert_equal([:IDENTIFIER, :"=", :NUMBER, :"\n", :EOF], tokens.map(&:type))
    end

    def test_nil
      tokens = Lexer.tokenize(<<~WID)
        nil
      WID

      assert_equal([:NIL, :"\n", :EOF], tokens.map(&:type))
    end

    def test_puts_method_call
      tokens = Lexer.tokenize(<<~WID)
        puts("Hello, World!")
      WID

      assert_equal([:IDENTIFIER, :"(", :STRING, :")", :"\n", :EOF], tokens.map(&:type))

      assert_equal("puts", tokens[0].value)
      assert_equal("\"Hello, World!\"", tokens[2].value)
    end
  end
end
