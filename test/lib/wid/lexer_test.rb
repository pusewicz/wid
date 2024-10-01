# frozen_string_literal: true

module Wid
  class LexerTest < Test
    def test_tokenize
      tokens = Lexer.tokenize(<<~WID)
        1 + 2.0
      WID

      assert_equal([:NUMBER, :"+", :NUMBER, :"\n", :EOF], tokens.map(&:type))
    end

    def test_new_line
      tokens = Lexer.tokenize(<<~WID)
        1 + 2
        3
      WID

      assert_equal([:NUMBER, :"+", :NUMBER, :"\n", :NUMBER, :"\n", :EOF], tokens.map(&:type))
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
