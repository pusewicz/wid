module Wid
  module AST
    class Node
      class Statements < Node
        attr_reader :body

        def initialize(body:)
          @body = body
        end

        def to_h
          super.merge(body: @body.map(&:to_h))
        end
      end
    end
  end
end
